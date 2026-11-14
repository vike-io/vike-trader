//! The `clap` types of `args.rs` held against the vocabularies they must agree with, and the rules
//! decision 0155 gives every command line, pinned on this program.
use super::search_flags::{argv, parsed, run_of};
use super::*;
use crate::backtest_cli::args::BacktestArgs;
use clap::Command;
use vike_model::run_request::flag_vocab::{BACKTEST_FLAGS, EXCLUDED, FlagArity};

/// The program's command, built (so the automatic `--help`/`--version` flags exist).
fn built() -> Command {
    let version = version_line();
    let mut command = program(&version).command::<BacktestArgs>();
    command.build();
    command
}

fn root_arg<'a>(command: &'a Command, long: &str) -> Option<&'a clap::Arg> {
    command.get_arguments().find(|a| a.get_long() == Some(long))
}

/// ⚠ **`vike_model::run_request::flag_vocab` is the flag VOCABULARY `vike-cli backtest run` and
/// this binary share, and this struct is the engine's reading of it.** Every row must be declared
/// here with the arity the row says, a roster must be the flag's value name (so `--help` and a
/// "value required" refusal both list it), and every EXCLUDED spelling must still be declared (it
/// is refused or special-cased by name, never "unknown"). The reverse holds too: a visible flag
/// the struct declares must be in the vocabulary, excluded from it with a reason, or one of the
/// two engine-only flags the vocabulary does not carry.
#[test]
fn the_clap_struct_declares_every_flag_the_vocabulary_names() {
    let command = built();
    for row in BACKTEST_FLAGS {
        let long = row.flag.trim_start_matches("--");
        let arg = root_arg(&command, long)
            .unwrap_or_else(|| panic!("{} is a vocabulary row and the struct lacks it", row.flag));
        match row.arity {
            FlagArity::Bare => {
                assert!(!arg.get_action().takes_values(), "{} is a bare switch", row.flag)
            }
            FlagArity::Valued | FlagArity::OptionalValue => {
                assert!(arg.get_action().takes_values(), "{} takes a value", row.flag)
            }
        }
        if !row.values.is_empty() {
            let names: Vec<String> =
                arg.get_value_names().unwrap_or(&[]).iter().map(|n| n.to_string()).collect();
            assert_eq!(
                names,
                vec![row.values.join("|")],
                "{}'s value name must be its roster, so the help and the refusal list it",
                row.flag
            );
        }
    }
    for (flag, _why) in EXCLUDED {
        let long = flag.trim_start_matches("--");
        assert!(root_arg(&command, long).is_some(), "{flag} is excluded from the table, not gone");
    }
    // The reverse: nothing visible is undeclared in the vocabulary.
    let engine_only_not_in_the_vocabulary = ["named-run"];
    for arg in command.get_arguments().filter(|a| !a.is_hide_set() && !a.is_positional()) {
        let long = arg.get_long().expect("a flag has a long name");
        let spelled = format!("--{long}");
        let known = BACKTEST_FLAGS.iter().any(|r| r.flag == spelled)
            || EXCLUDED.iter().any(|(f, _)| *f == spelled)
            || engine_only_not_in_the_vocabulary.contains(&long);
        assert!(known, "{spelled} is declared by the struct and named by no vocabulary");
    }
}

/// The rules of the one table, on the run verb: a repeated flag, a switch with a value, an unknown
/// flag (with its suggestion) and a blank value are all refusals.
#[test]
fn the_rules_every_command_line_obeys_hold_for_the_run_verb() {
    let refused = |words: &[&str]| parsed(&argv(words)).expect_err(&format!("{words:?}"));
    assert!(
        refused(&["p.toml", "--rank-by", "return", "--rank-by", "sharpe"]).contains("--rank-by")
    );
    assert!(refused(&["p.toml", "--json=1"]).contains("--json"));
    assert!(refused(&["p.toml", "--list=1"]).contains("--list"));
    let typo = refused(&["p.toml", "--optimzer", "tpe"]);
    assert!(typo.contains("--optimzer") && typo.contains("--optimizer"), "a suggestion: {typo}");
    assert!(refused(&["p.toml", "--seed="]).contains("non-empty"));
    assert!(refused(&["--profile", " "]).contains("non-empty"));
    // A negative number is a value; a single-dash word is not.
    assert!(parsed(&argv(&["p.toml", "--seed", "-5"])).is_ok());
    assert!(parsed(&argv(&["p.toml", "--seed", "-foo"])).is_err());
}

/// `--help` and `--version` are exits, not errors, on the root and on a verb.
#[test]
fn help_and_version_are_exits_with_code_zero() {
    for words in [&["--help"][..], &["-h"], &["trials", "--help"], &["trials", "-h"]] {
        let version = version_line();
        let exit = program(&version)
            .try_parse_from::<BacktestArgs, _, _>(&argv(words))
            .expect_err("help is an exit");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{words:?}");
    }
    for words in [&["--version"][..], &["-V"], &["trials", "--version"]] {
        let version = version_line();
        let exit = program(&version)
            .try_parse_from::<BacktestArgs, _, _>(&argv(words))
            .expect_err("version is an exit");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{words:?}");
        assert_eq!(exit.text, format!("backtesthub {}\n", env!("CARGO_PKG_VERSION")), "{words:?}");
    }
}

/// `--named-run` is the lane's own flag, spelled once by `vike-backtest`.
#[test]
fn the_named_run_switch_is_the_lanes_flag() {
    assert_eq!(crate::named_run::NAMED_RUN_FLAG, "--named-run");
    assert!(run_of(&argv(&[crate::named_run::NAMED_RUN_FLAG])).unwrap().named_run);
    assert!(!run_of(&argv(&[])).unwrap().named_run);
}

/// `trials` routes only in FIRST position: after a flag it is the profile word, which
/// `profile_from_args` refuses by name. `data` is a plain profile word since the store verbs left.
#[test]
fn a_verb_word_routes_only_in_first_position() {
    assert!(matches!(parsed(&argv(&["trials", "a-b"])).unwrap().verb, Some(Verb::Trials(_))));
    assert!(parsed(&argv(&["data"])).unwrap().verb.is_none(), "`data` is no verb here");
    let after_a_flag = parsed(&argv(&["--json", "trials"])).unwrap();
    assert!(after_a_flag.verb.is_none(), "trials after a flag is not a verb");
    assert_eq!(after_a_flag.run.profile, vec!["trials".to_string()]);
    // `help` is a profile word on the root (the help subcommand is disabled there).
    assert_eq!(run_of(&argv(&["help"])).unwrap().profile, vec!["help".to_string()]);
}
