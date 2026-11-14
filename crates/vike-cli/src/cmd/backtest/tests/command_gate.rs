//! The `clap` command and the table `crate::surface` publishes must describe the SAME plane (decision
//! 0155). What the table holds is what `clap` cannot know; what `clap` knows comes from the command
//! (`FlagRow::value`, `FlagRow::repeat`, `sub_verbs`), and `surface_tests.rs`'s
//! `every_flag_of_the_plane_is_classified_exactly_once` holds the row set to the flag set both ways.
//!
//! `the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb` measures the table's `applies_to`
//! against the parsers by DRIVING them. This gate measures it against the command's own description —
//! `vike_utility::cli::describe` over [`command`] — which is what `--help` and the published `commands`
//! array are rendered from:
//!
//! * every flag the table says SHIPS on a sub-verb exists on that sub-verb of the command, with the
//!   arity and the repeatability published for it;
//! * every `refused` row is refused by the real parser, given twice, on every sub-verb it ships on,
//!   and every repeatable row is not.

use vike_utility::cli::{FlagRow, describe};

use super::*;

use crate::surface::{FLAGS, Repeat, Status, Value};

fn rows() -> Vec<FlagRow> {
    describe(&command())
}

/// The command's rows for one sub-verb: its OWN flags, `--help`/`--version` excluded by `describe`.
fn rows_of<'a>(all: &'a [FlagRow], sub: &str) -> Vec<&'a FlagRow> {
    all.iter().filter(|r| r.command == [sub.to_string()] && !r.positional).collect()
}

#[test]
fn every_flag_the_table_ships_exists_on_its_sub_verbs_with_the_claimed_arity() {
    let all = rows();
    let mut wrong: Vec<String> = Vec::new();
    for flag in FLAGS.iter().filter(|f| f.status == Status::Ships && f.long != "--help") {
        let name = flag.long.trim_start_matches("--");
        for sub in flag.applies_to {
            let found = rows_of(&all, sub).into_iter().find(|r| r.name == name);
            match found {
                None => {
                    wrong.push(format!("{} is not a flag of `{sub}` in the command", flag.long))
                }
                Some(row) => {
                    let takes = flag.value() == Value::Required;
                    if row.takes_value != takes {
                        wrong.push(format!(
                            "{} on `{sub}`: the table says takes_value={takes}, the command {}",
                            flag.long, row.takes_value
                        ));
                    }
                    let repeats =
                        matches!(flag.repeat(), Repeat::Accumulate | Repeat::CommaListFold);
                    if repeats && !row.repeatable {
                        wrong.push(format!(
                            "{} on `{sub}`: the table says it repeats, the command refuses a repeat",
                            flag.long
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "the table and the command disagree:\n  {}", wrong.join("\n  "));
}

/// `run` declares every row of [`crate::cmd::backtest::SUGAR`] — the table that says which profile
/// key each sugar flag lands on. A row added there without a field on the struct (or the reverse)
/// is a flag nothing reads or a key nothing can reach.
#[test]
fn the_run_command_declares_every_sugar_flag() {
    let all = rows();
    let run: std::collections::BTreeSet<String> =
        rows_of(&all, "run").iter().map(|r| format!("--{}", r.name)).collect();
    for s in SUGAR.iter() {
        assert!(
            run.contains(s.flag),
            "`run` does not declare {}, which SUGAR maps to {}",
            s.flag,
            s.key
        );
    }
}

/// `repeat` as published is what the real parser does: `refused` rows are refused given twice, on
/// every sub-verb they ship on, and the repeatable rows are not. The rows said `last_wins` until
/// `cli.json` caught up with `clap`, which refuses what the hand-rolled loops overwrote silently.
#[test]
fn a_published_repeat_is_what_the_parser_does() {
    let mut wrong: Vec<String> = Vec::new();
    for flag in FLAGS.iter().filter(|f| f.status == Status::Ships && f.value() == Value::Required) {
        let sample = flag.sample.expect("a value-taking row carries a sample");
        for sub in flag.applies_to {
            let twice = [*sub, flag.long, sample, flag.long, sample].map(str::to_string);
            let refused = match parse(twice) {
                Ok(_) => false,
                Err(stop) => {
                    assert!(
                        stop.contains("cannot be used multiple times"),
                        "{} on `{sub}` is refused for another reason: {stop}",
                        flag.long
                    );
                    true
                }
            };
            if refused != (flag.repeat() == Repeat::Refused) {
                wrong.push(format!(
                    "{} on `{sub}`: published {:?}, the parser refused={refused}",
                    flag.long,
                    flag.repeat()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "the published `repeat` is false:\n  {}", wrong.join("\n  "));
}
