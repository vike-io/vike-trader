//! The `clap` types of `args.rs` held against the vocabularies they must agree with, and the rules
//! decision 0155 gives every command line, pinned on this program.
use super::search_flags::{argv, parsed, run_of};
use super::*;
use crate::backtest_cli::args::{BacktestArgs, DataSub};
use clap::Command;
use vike_model::run_request::flag_vocab::{Arity, BACKTEST_FLAGS, EXCLUDED};

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
            Arity::Bare => {
                assert!(!arg.get_action().takes_values(), "{} is a bare switch", row.flag)
            }
            Arity::Valued | Arity::OptionalValue => {
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
    let engine_only_not_in_the_vocabulary = ["named-run", "archive"];
    for arg in command.get_arguments().filter(|a| !a.is_hide_set() && !a.is_positional()) {
        let long = arg.get_long().expect("a flag has a long name");
        let spelled = format!("--{long}");
        let known = BACKTEST_FLAGS.iter().any(|r| r.flag == spelled)
            || EXCLUDED.iter().any(|(f, _)| *f == spelled)
            || engine_only_not_in_the_vocabulary.contains(&long);
        assert!(known, "{spelled} is declared by the struct and named by no vocabulary");
    }
}

/// The retired companions are exactly the `data` flags minus the two the run verb owns, so an old
/// cleanup script reaches its refusal whatever it passed.
#[test]
fn the_retired_companions_are_the_data_flags_the_run_verb_does_not_own() {
    let command = built();
    let data = command.find_subcommand("data").expect("the data verb");
    let seed_demo = data.find_subcommand("seed-demo").expect("seed-demo");
    let mut data_longs: Vec<&str> = seed_demo
        .get_arguments()
        .filter_map(|a| a.get_long())
        .filter(|l| !["help", "version"].contains(l))
        .collect();
    data_longs.sort_unstable();
    let companion_longs = {
        let mut out: Vec<&str> = command
            .get_arguments()
            .filter(|a| a.is_hide_set())
            .filter_map(|a| a.get_long())
            .filter(|l| data_longs.contains(l) && !["store", "json"].contains(l))
            .collect();
        out.sort_unstable();
        out
    };
    let expected: Vec<&str> =
        data_longs.iter().copied().filter(|l| !["store", "json"].contains(l)).collect();
    assert_eq!(companion_longs, expected);
}

/// Every retired flag names a live replacement and an engine subcommand the `clap` command serves.
#[test]
fn every_retired_data_flag_names_its_replacement_and_a_real_subcommand() {
    let command = built();
    let data = command.find_subcommand("data").expect("the data verb");
    for (flag, sub, replacement, _) in data_cmd::RETIRED_DATA_FLAGS {
        assert!(
            replacement.starts_with("vike-cli data hist "),
            "{flag} must name the verb the ruling moved it to, in the GROUPED spelling \
             `vike-cli` accepts, got {replacement:?}"
        );
        assert!(
            data.find_subcommand(sub).is_some(),
            "{flag} names `backtest data {sub}`, which this binary does not serve"
        );
        for line in [vec![(*flag).to_string()], vec![format!("{flag}=x")]] {
            let run = run_of(&line).expect("a retired flag parses so its refusal is reached");
            assert!(
                data_cmd::refuse_a_retired_data_flag(&run).is_some(),
                "{line:?} must be refused, not read as absent"
            );
        }
    }
    let run = run_of(&argv(&["--json"])).unwrap();
    assert!(
        data_cmd::refuse_a_retired_data_flag(&run).is_none(),
        "an unrelated flag falls through"
    );
}

/// ⚠ The adjacent-prefix pair, pinned: `--fetch-starter` must refuse under its OWN row, naming its
/// own replacement, rather than under `--fetch`'s.
#[test]
fn fetch_starter_is_not_swallowed_by_the_fetch_row() {
    let run = run_of(&argv(&["--fetch-starter"])).unwrap();
    let hit = data_cmd::RETIRED_DATA_FLAGS
        .iter()
        .find(|(f, ..)| data_cmd::retired_written(&run, f).is_some());
    assert_eq!(hit.map(|(f, ..)| *f), Some("--fetch-starter"));
}

/// Every reader row names a real subcommand, so the refusal cannot guard a verb that does not
/// exist while the real one reads a directory.
#[test]
fn every_data_read_is_a_real_data_subcommand() {
    let command = built();
    let data = command.find_subcommand("data").expect("the data verb");
    for read in DATA_READS {
        assert!(data.find_subcommand(read).is_some(), "DATA_READS names `{read}`");
    }
}

/// `--store` on the READ verb is refused BY NAME, in both spellings, and a WRITER keeps it: the
/// ruling was about readers.
#[test]
fn the_store_flag_on_a_data_read_is_refused_by_name_and_a_writer_keeps_it() {
    for written in [
        vec!["data", "export", "demo:DEMOUSDT:1h", "--out", "x.parquet", "--store", "/srv/hist"],
        vec!["data", "export", "demo:DEMOUSDT:1h", "--out", "x.parquet", "--store=/srv/hist"],
        // A TRAILING value-less `--store`: the refusal by name comes first, not "a value is required".
        vec!["data", "export", "demo:DEMOUSDT:1h", "--out", "x.parquet", "--store"],
    ] {
        let Some(Verb::Data { sub: Some(DataSub::Export(spec)) }) =
            parsed(&argv(&written)).expect("parses").verb
        else {
            panic!("{written:?} is not an export");
        };
        let why = data_cmd::refuse_a_store_on_a_data_read("export", &spec.flags)
            .unwrap_or_else(|| panic!("{written:?} must be refused"));
        assert!(why.starts_with("backtest data export:"), "names the verb as typed: {why}");
        assert!(why.contains("vike-backend datahub --store DIR"), "the replacement: {why}");
        assert!(why.contains("decision 0084"), "the record, by number: {why}");
    }
    let Some(Verb::Data { sub: Some(DataSub::Export(spec)) }) =
        parsed(&argv(&["data", "export", "demo:D:1h", "--out", "x"])).unwrap().verb
    else {
        panic!("an export")
    };
    assert!(data_cmd::refuse_a_store_on_a_data_read("export", &spec.flags).is_none());
    let Some(Verb::Data { sub: Some(DataSub::SeedDemo(flags)) }) =
        parsed(&argv(&["data", "seed-demo", "--store", "/srv/hist"])).unwrap().verb
    else {
        panic!("a seed-demo")
    };
    assert!(
        data_cmd::refuse_a_store_on_a_data_read("seed-demo", &flags).is_none(),
        "`data seed-demo` WRITES and keeps its --store"
    );
}

/// The shape rules the hand-rolled triage enforced, now `clap`'s: an unknown option, a boolean
/// given a value, a valued flag given none, a positional where the subcommand takes none and
/// missing where it needs one.
#[test]
fn the_shape_of_a_data_command_line_is_refused_by_clap() {
    let refused = |words: &[&str]| parsed(&argv(words)).expect_err(&format!("{words:?}"));
    assert!(refused(&["data", "rm", "--bogus"]).contains("--bogus"));
    assert!(refused(&["data", "rm", "--kind"]).contains("--kind"));
    assert!(refused(&["data", "rm", "--kind", "--venue"]).contains("--kind"));
    assert!(refused(&["data", "export", "--out", "x"]).to_lowercase().contains("required"));
    assert!(refused(&["data", "seed-demo", "x:y:z"]).contains("<hidden>"), "never echoed");
    assert!(refused(&["data", "nope"]).contains("nope"));
    // ⚠ **The `--dry-run=1` hole**: it silently ran a deletion written as a rehearsal.
    for spelling in ["--dry-run=1", "--yes=true", "--json=1"] {
        let text = refused(&["data", "rm", "--kind", "bar", "--venue", "x", spelling]);
        assert!(text.contains(spelling.split('=').next().unwrap()), "{spelling}: {text}");
    }
    // …and a value may be a negative number (`--days -4`), where a flag may not be a value.
    let ok = parsed(&argv(&["data", "fetch", "nope:NOPE:99z", "--days", "-4"]));
    assert!(ok.is_ok(), "{ok:?}");
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
    for words in [&["--help"][..], &["-h"], &["data", "--help"], &["trials", "-h"]] {
        let version = version_line();
        let exit = program(&version)
            .try_parse_from::<BacktestArgs, _, _>(&argv(words))
            .expect_err("help is an exit");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{words:?}");
    }
    for words in [&["--version"][..], &["-V"], &["data", "--version"]] {
        let version = version_line();
        let exit = program(&version)
            .try_parse_from::<BacktestArgs, _, _>(&argv(words))
            .expect_err("version is an exit");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{words:?}");
        assert_eq!(exit.text, format!("backtest {}\n", env!("CARGO_PKG_VERSION")), "{words:?}");
    }
}

/// `--named-run` is the lane's own flag, spelled once by `vike-backtest`.
#[test]
fn the_named_run_switch_is_the_lanes_flag() {
    assert_eq!(vike_backtest::named_run::NAMED_RUN_FLAG, "--named-run");
    assert!(run_of(&argv(&[vike_backtest::named_run::NAMED_RUN_FLAG])).unwrap().named_run);
    assert!(!run_of(&argv(&[])).unwrap().named_run);
}

/// `data` and `trials` route only in FIRST position: after a flag they are the profile word, which
/// `profile_from_args` refuses by name.
#[test]
fn a_verb_word_routes_only_in_first_position() {
    assert!(matches!(parsed(&argv(&["data", "seed-demo"])).unwrap().verb, Some(Verb::Data { .. })));
    assert!(matches!(parsed(&argv(&["trials", "a-b"])).unwrap().verb, Some(Verb::Trials(_))));
    for word in ["data", "trials"] {
        let parsed = parsed(&argv(&["--json", word])).unwrap();
        assert!(parsed.verb.is_none(), "{word} after a flag is not a verb");
        assert_eq!(parsed.run.profile, vec![word.to_string()]);
    }
    // `help` is a profile word on the root (the help subcommand is disabled there).
    assert_eq!(run_of(&argv(&["help"])).unwrap().profile, vec!["help".to_string()]);
}
