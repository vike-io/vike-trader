//! The rule table of decision 0155 (verdict 3), one test per row, on small `clap` derives defined
//! here, plus the secret-hygiene rows, `describe`/`commands` and `check_examples`.
//!
//! Where `clap`'s own behaviour is the rule, the test pins what `clap` ACTUALLY does (it was read
//! off `clap` 4.6.7, not remembered); where it is not, the test says so and pins the divergence.

use clap::Parser;

use super::{CliExit, Program, USAGE_EXIT, check_examples, commands, describe, non_empty};

/// A program with every argument shape the rule table talks about.
#[derive(Parser, Debug, PartialEq)]
struct Demo {
    /// the name of the thing
    #[arg(long)]
    name: Option<String>,
    /// a label that is trimmed
    #[arg(long, value_parser = non_empty)]
    label: Option<String>,
    /// print JSON
    #[arg(long)]
    json: bool,
    #[arg(long)]
    verbose: bool,
    /// repeatable
    #[arg(long)]
    add: Vec<String>,
    #[arg(long)]
    from: Option<i64>,
    #[arg(long, default_value_t = 7)]
    depth: u32,
    /// the one positional
    file: Option<String>,
}

#[derive(Parser, Debug, PartialEq)]
struct Verbs {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(clap::Subcommand, Debug, PartialEq)]
enum Verb {
    /// run a backtest
    Backtest {
        /// first bar offset
        #[arg(long, default_value_t = 0)]
        offset: i64,
        #[arg(long)]
        tag: Option<String>,
    },
    /// show the status
    Status,
}

#[derive(Parser, Debug, PartialEq)]
struct Needs {
    #[arg(long)]
    must: String,
}

/// A program that reads a credential and takes no positional at all.
#[derive(Parser, Debug, PartialEq)]
struct Authorize {
    #[arg(long)]
    client_id: Option<String>,
    #[arg(long, value_parser = ["paper", "demo", "live"])]
    tier: Option<String>,
}

#[derive(Parser, Debug, PartialEq)]
struct Numbered {
    /// a count
    n: Option<u32>,
}

const VERSION_LINE: &str = "demo 1.2.3 (abc1234)";

fn prog() -> Program<'static> {
    Program::new("demo", VERSION_LINE)
}

fn demo(args: &[&str]) -> Result<Demo, CliExit> {
    prog().try_parse_from::<Demo, _, _>(args.iter().copied())
}

/// The refusal, which the test expects: the exit is an error on stderr with code 2 whose text
/// starts with `<bin>: `.
fn refused<T: std::fmt::Debug>(result: Result<T, CliExit>) -> String {
    let exit = result.expect_err("the command line must be refused");
    assert_eq!(exit.code, USAGE_EXIT, "every command-line error exits 2: {exit:?}");
    assert!(!exit.to_stdout, "an error goes to stderr: {exit:?}");
    assert!(exit.text.starts_with("demo: "), "the text starts with `<bin>: `: {:?}", exit.text);
    assert!(
        !exit.text.contains("error:"),
        "clap's own `error:` prefix is replaced: {:?}",
        exit.text
    );
    exit.text
}

// --- rule 1: `--f v` and `--f=v` -------------------------------------------------------------

#[test]
fn both_spellings_of_a_value_are_read() {
    let spaced = demo(&["--name", "v"]).unwrap();
    let equals = demo(&["--name=v"]).unwrap();
    assert_eq!(spaced.name.as_deref(), Some("v"));
    assert_eq!(spaced, equals);
}

#[test]
fn the_equals_form_splits_on_the_first_equals_only() {
    assert_eq!(demo(&["--name=a=b"]).unwrap().name.as_deref(), Some("a=b"));
}

#[test]
fn a_default_is_used_when_the_flag_is_absent() {
    let parsed = demo(&[]).unwrap();
    assert_eq!(parsed.depth, 7);
    assert_eq!(parsed.name, None);
    assert!(!parsed.json);
}

// --- rule 2: an unknown flag is refused, with a suggestion -----------------------------------

#[test]
fn an_unknown_flag_is_refused_with_a_suggestion() {
    let text = refused(demo(&["--verbos"]));
    assert!(text.contains("unexpected argument '--verbos' found"), "{text}");
    assert!(text.contains("a similar argument exists: '--verbose'"), "{text}");
    assert!(text.contains("Usage: demo"), "the usage follows the message: {text}");
}

#[test]
fn an_unknown_flag_with_no_close_name_is_refused_without_a_suggestion() {
    let text = refused(demo(&["--zzzzzzzz"]));
    assert!(text.contains("unexpected argument '--zzzzzzzz' found"), "{text}");
    assert!(!text.contains("similar argument"), "{text}");
}

#[test]
fn an_abbreviated_long_flag_is_not_accepted() {
    // `infer_long_args` is off: `--verb` is not `--verbose`.
    refused(demo(&["--verb"]));
}

// --- rule 3 + the single-dash row: missing values, negative numbers --------------------------

#[test]
fn a_flag_without_its_value_is_refused() {
    let text = refused(demo(&["--name"]));
    assert!(
        text.contains("a value is required for '--name <NAME>' but none was supplied"),
        "{text}"
    );
}

#[test]
fn a_following_flag_is_not_swallowed_as_the_value() {
    let text = refused(demo(&["--name", "--verbose"]));
    assert!(
        text.contains("a value is required for '--name <NAME>' but none was supplied"),
        "{text}"
    );
}

#[test]
fn a_negative_number_is_a_value() {
    assert_eq!(demo(&["--from", "-5"]).unwrap().from, Some(-5));
    assert_eq!(demo(&["--from=-5"]).unwrap().from, Some(-5));
    assert_eq!(demo(&["-5"]).unwrap().file.as_deref(), Some("-5"));
}

#[test]
fn clap_alone_reads_a_negative_number_as_a_flag() {
    // The reason `Program::command` sets `allow_negative_numbers` on every value argument: without
    // it `--from -5` is `--from` with no value followed by an unknown short flag `-5`.
    let raw = <Demo as Parser>::try_parse_from(["demo", "--from", "-5"]);
    assert!(raw.is_err(), "{raw:?}");
}

#[test]
fn a_negative_number_is_a_value_inside_a_subcommand_too() {
    let parsed = prog().try_parse_from::<Verbs, _, _>(["backtest", "--offset", "-3"]).unwrap();
    assert_eq!(parsed.verb, Verb::Backtest { offset: -3, tag: None });
}

/// DIVERGENCE from decision 0155 verdict 3 ("a single-dash token that is not a declared short flag
/// is a value"): only a NEGATIVE NUMBER is. `allow_hyphen_values` would make every such token a
/// value, but it also makes `--name --verbose` read `--verbose` as `--name`'s value (it overrides
/// the "next token is a flag" refusal), which the rule table forbids too; clap cannot do both.
#[test]
fn a_single_dash_word_is_still_read_as_a_flag() {
    let text = refused(demo(&["--name", "-foo"]));
    assert!(text.contains("unexpected argument '-f' found"), "{text}");
}

// --- rule 4: a repeated flag --------------------------------------------------------------------

#[test]
fn a_repeated_value_flag_is_refused() {
    let text = refused(demo(&["--name", "a", "--name", "b"]));
    assert!(text.contains("the argument '--name <NAME>' cannot be used multiple times"), "{text}");
}

#[test]
fn a_repeated_switch_is_refused() {
    let text = refused(demo(&["--json", "--json"]));
    assert!(text.contains("the argument '--json' cannot be used multiple times"), "{text}");
}

#[test]
fn a_vec_field_is_repeatable_and_keeps_the_order() {
    let parsed = demo(&["--add", "a", "--add=b", "--add", "c"]).unwrap();
    assert_eq!(parsed.add, ["a", "b", "c"]);
}

// --- rule 5: a switch with a value ------------------------------------------------------------

#[test]
fn a_switch_with_a_value_is_refused() {
    for spelled in ["--json=1", "--json=true", "--json="] {
        let text = refused(demo(&[spelled]));
        assert!(text.contains("--json"), "{spelled}: {text}");
    }
    let text = refused(demo(&["--json=1"]));
    assert!(
        text.contains("unexpected value '1' for '--json' found; no more were expected"),
        "{text}"
    );
}

#[test]
fn a_bare_switch_is_true() {
    assert!(demo(&["--json"]).unwrap().json);
}

// --- rule 6: a blank value -----------------------------------------------------------------------

#[test]
fn non_empty_trims_and_refuses_blank() {
    assert_eq!(non_empty("  x \t"), Ok("x".to_owned()));
    assert_eq!(non_empty(""), Err("needs a non-empty value".to_owned()));
    assert_eq!(non_empty(" \t "), Err("needs a non-empty value".to_owned()));
}

#[test]
fn non_empty_as_a_value_parser_trims_and_refuses() {
    assert_eq!(demo(&["--label", "  x "]).unwrap().label.as_deref(), Some("x"));
    let text = refused(demo(&["--label", " "]));
    assert!(text.contains("needs a non-empty value"), "{text}");
}

#[test]
fn a_blank_value_is_refused_for_every_flag_without_opting_in() {
    for args in [vec!["--name", ""], vec!["--name", "  "], vec!["--name="], vec!["--add", " "]] {
        let text = refused(demo(&args));
        assert!(
            text.contains("needs a non-empty value") || text.contains("a value is required"),
            "{args:?}: {text}"
        );
    }
}

#[test]
fn a_blank_positional_is_refused() {
    let text = refused(demo(&[""]));
    assert!(text.contains("<file> needs a non-empty value"), "{text}");
}

#[test]
fn a_blank_value_inside_a_subcommand_is_refused() {
    let exit = prog().try_parse_from::<Verbs, _, _>(["backtest", "--tag", " "]).unwrap_err();
    assert_eq!(exit.code, USAGE_EXIT);
    assert!(exit.text.contains("--tag needs a non-empty value"), "{}", exit.text);
}

// --- rule 7: help and version ------------------------------------------------------------------

#[test]
fn help_goes_to_stdout_with_exit_zero() {
    for flag in ["-h", "--help"] {
        let exit = demo(&[flag]).unwrap_err();
        assert_eq!(exit.code, 0, "{flag}");
        assert!(exit.to_stdout, "{flag}");
        assert!(exit.text.contains("Usage: demo"), "{flag}: {}", exit.text);
        assert!(exit.text.contains("the name of the thing"), "{flag}: {}", exit.text);
    }
}

#[test]
fn version_prints_the_callers_line_to_stdout_with_exit_zero() {
    for flag in ["-V", "--version"] {
        let exit = demo(&[flag]).unwrap_err();
        assert_eq!(
            exit,
            CliExit { code: 0, to_stdout: true, text: format!("{VERSION_LINE}\n") },
            "{flag}"
        );
    }
}

#[test]
fn help_and_version_work_on_a_subcommand() {
    let help = prog().try_parse_from::<Verbs, _, _>(["backtest", "--help"]).unwrap_err();
    assert_eq!((help.code, help.to_stdout), (0, true));
    assert!(help.text.contains("Usage: demo backtest"), "{}", help.text);
    let version = prog().try_parse_from::<Verbs, _, _>(["backtest", "-V"]).unwrap_err();
    assert_eq!(version.text, format!("{VERSION_LINE}\n"));
}

// --- rule 8: every other error is `<bin>: <message>` on stderr, exit 2 -----------------------------

#[test]
fn a_missing_required_argument_exits_two_with_the_bin_prefix() {
    let text = refused(prog().try_parse_from::<Needs, _, _>(Vec::<&str>::new()));
    assert!(text.contains("the following required arguments were not provided"), "{text}");
    assert!(text.contains("--must <MUST>"), "{text}");
}

#[test]
fn an_error_ends_with_the_help_pointer() {
    let text = refused(demo(&["--nope"]));
    assert!(text.ends_with("For more information, try '--help'.\n"), "{text}");
}

#[test]
fn a_missing_subcommand_is_an_error_not_a_panic() {
    let exit = prog().try_parse_from::<Verbs, _, _>(Vec::<&str>::new()).unwrap_err();
    assert_eq!(exit.code, USAGE_EXIT);
    assert!(!exit.to_stdout);
}

// --- rule 9: positionals --------------------------------------------------------------------------

#[test]
fn a_surplus_positional_is_refused_and_not_echoed() {
    let text = refused(demo(&["first", "stray-positional-9"]));
    assert!(text.contains("unexpected argument '<hidden>' found"), "{text}");
    assert!(!text.contains("stray-positional-9"), "{text}");
}

#[test]
fn the_declared_positional_is_read() {
    assert_eq!(demo(&["a.csv"]).unwrap().file.as_deref(), Some("a.csv"));
}

// --- secret hygiene: a positional is never echoed --------------------------------------------------

#[test]
fn a_stray_positional_never_reaches_the_error_text() {
    let secret = "hunter2-s3cret-token";
    let exit = prog().try_parse_from::<Authorize, _, _>([secret]).unwrap_err();
    assert_eq!(exit.code, USAGE_EXIT);
    assert!(!exit.text.contains("hunter2"), "{}", exit.text);
    assert!(!exit.text.contains(secret), "{}", exit.text);
    assert!(exit.text.contains("unexpected argument '<hidden>' found"), "{}", exit.text);
    // Nor in clap's `to pass 'x' as a value, use '-- x'` tip, which carries the token verbatim.
    assert!(!exit.text.contains("-- hunter2"), "{}", exit.text);
}

#[test]
fn a_secret_after_an_unknown_flag_is_not_echoed() {
    let exit = prog()
        .try_parse_from::<Authorize, _, _>(["--clientsecret", "hunter2-s3cret-token"])
        .unwrap_err();
    assert!(!exit.text.contains("hunter2"), "{}", exit.text);
    let exit = prog()
        .try_parse_from::<Authorize, _, _>(["--client-secret=hunter2-s3cret-token"])
        .unwrap_err();
    assert!(!exit.text.contains("hunter2"), "{}", exit.text);
    assert!(exit.text.contains("--client-secret"), "the flag NAME is kept: {}", exit.text);
}

#[test]
fn a_positional_that_fails_its_parser_is_not_echoed() {
    let exit = prog().try_parse_from::<Numbered, _, _>(["hunter2-s3cret-token"]).unwrap_err();
    assert_eq!(exit.code, USAGE_EXIT);
    assert!(!exit.text.contains("hunter2"), "{}", exit.text);
    assert!(exit.text.contains("invalid value '<hidden>'"), "{}", exit.text);
}

#[test]
fn a_flag_value_is_echoed_by_default_and_hidden_on_request() {
    let secret = "hunter2-s3cret-token";
    let shown = prog().try_parse_from::<Authorize, _, _>(["--tier", secret]).unwrap_err();
    assert!(
        shown.text.contains("invalid value 'hunter2-s3cret-token' for '--tier <TIER>'"),
        "{}",
        shown.text
    );
    assert!(shown.text.contains("[possible values: paper, demo, live]"), "{}", shown.text);

    let hidden =
        prog().hide_values().try_parse_from::<Authorize, _, _>(["--tier", secret]).unwrap_err();
    assert!(!hidden.text.contains("hunter2"), "{}", hidden.text);
    assert!(
        hidden.text.contains("invalid value '<hidden>' for '--tier <TIER>'"),
        "{}",
        hidden.text
    );
    assert!(hidden.text.contains("[possible values: paper, demo, live]"), "{}", hidden.text);
}

#[test]
fn hide_values_also_hides_a_token_that_only_starts_with_a_dash() {
    let exit = prog()
        .hide_values()
        .try_parse_from::<Authorize, _, _>(["-----BEGIN+hunter2/KEY"])
        .unwrap_err();
    assert!(!exit.text.contains("hunter2"), "{}", exit.text);
}

#[test]
fn hide_values_keeps_flag_names_and_hides_a_mistyped_verb() {
    let exit =
        prog().hide_values().try_parse_from::<Authorize, _, _>(["--cliant-id", "x"]).unwrap_err();
    assert!(exit.text.contains("'--cliant-id'"), "{}", exit.text);
    let exit = prog().hide_values().try_parse_from::<Verbs, _, _>(["hunter2"]).unwrap_err();
    assert!(!exit.text.contains("hunter2"), "{}", exit.text);
}

#[test]
fn a_mistyped_verb_is_echoed_with_a_suggestion_by_default() {
    let exit = prog().try_parse_from::<Verbs, _, _>(["bakctest"]).unwrap_err();
    assert!(exit.text.contains("unrecognized subcommand 'bakctest'"), "{}", exit.text);
    assert!(exit.text.contains("a similar subcommand exists: 'backtest'"), "{}", exit.text);
}

// --- the plain-data walk ----------------------------------------------------------------------------

#[test]
fn describe_lists_every_argument_with_its_shape() {
    let rows = describe(&prog().command::<Demo>());
    let row =
        |name: &str| rows.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("no {name}"));

    let name = row("name");
    assert!(name.takes_value && !name.required && !name.repeatable && !name.positional);
    assert_eq!(name.help, "the name of the thing");
    assert!(name.command.is_empty());

    let json = row("json");
    assert!(!json.takes_value);

    let add = row("add");
    assert!(add.repeatable && add.takes_value);

    let depth = row("depth");
    assert_eq!(depth.default, ["7"]);

    let file = row("file");
    assert!(file.positional && file.takes_value);

    assert!(rows.iter().all(|r| r.name != "help" && r.name != "version"));
}

#[test]
fn describe_marks_a_required_argument_and_possible_values() {
    let needs = describe(&prog().command::<Needs>());
    assert!(needs.iter().find(|r| r.name == "must").unwrap().required);

    let auth = describe(&prog().command::<Authorize>());
    let tier = auth.iter().find(|r| r.name == "tier").unwrap();
    assert_eq!(tier.possible_values, ["paper", "demo", "live"]);
}

#[test]
fn describe_and_commands_walk_subcommands_with_their_path() {
    let cmd = prog().command::<Verbs>();
    let rows = describe(&cmd);
    let offset = rows.iter().find(|r| r.name == "offset").unwrap();
    assert_eq!(offset.command, ["backtest"]);
    assert_eq!(offset.default, ["0"]);
    assert_eq!(offset.help, "first bar offset");

    let listed: Vec<(Vec<String>, String)> =
        commands(&cmd).into_iter().map(|c| (c.path, c.about)).collect();
    assert_eq!(listed[1], (vec!["backtest".to_owned()], "run a backtest".to_owned()));
    assert_eq!(listed[2], (vec!["status".to_owned()], "show the status".to_owned()));
    assert_eq!(listed[0].0, Vec::<String>::new());
}

// --- documented examples ----------------------------------------------------------------------------

#[test]
fn check_examples_accepts_shell_style_lines_without_running_them() {
    let cmd = prog().command::<Verbs>();
    let lines = [
        "demo backtest --offset -3 --tag 'a b'   # a trailing comment",
        "# a comment only line",
        "",
        "$ demo status",
        "demo backtest \\",
        "    --offset=4",
        "demo --help",
        "demo status | jq .",
        "demo status 2>&1",
        "demo backtest --tag \"say \\\"hi\\\"\"",
    ];
    assert_eq!(check_examples(&cmd, &lines), Ok(()));
}

#[test]
fn check_examples_names_every_refused_line() {
    let cmd = prog().command::<Verbs>();
    let lines = [
        "demo backtest --offsett 3",
        "demo bakctest",
        "demo backtest --tag ''",
        "other status",
        "demo backtest --tag 'unterminated",
        "demo status",
    ];
    let err = check_examples(&cmd, &lines).unwrap_err();
    assert!(err.contains("line 1: unexpected argument '--offsett' found"), "{err}");
    assert!(err.contains("line 2: unrecognized subcommand 'bakctest'"), "{err}");
    assert!(err.contains("line 3: --tag needs a non-empty value"), "{err}");
    assert!(err.contains("line 4: starts with `other`, not `demo`"), "{err}");
    assert!(err.contains("line 5: unterminated single quote"), "{err}");
    assert!(!err.contains("line 6"), "{err}");
    assert_eq!(err.lines().count(), 5, "{err}");
}

#[test]
fn check_examples_numbers_lines_across_a_continuation() {
    let cmd = prog().command::<Verbs>();
    let lines = ["demo status", "demo backtest \\", "  --offsett 3"];
    let err = check_examples(&cmd, &lines).unwrap_err();
    assert!(err.starts_with("line 2: "), "{err}");
}
