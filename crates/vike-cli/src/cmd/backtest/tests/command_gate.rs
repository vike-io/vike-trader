//! The `clap` command and the hand table `crate::surface` publishes must describe the SAME plane
//! (decision 0155: until the table is derived from the command, this gate is what stops the two
//! from drifting).
//!
//! `the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb` already measures the table
//! against the parsers by DRIVING them. This gate measures it against the command's own
//! description — `vike_util::cli::describe` over [`command`] — which is what `--help` and the future
//! generated reference are rendered from:
//!
//! * the sub-verb roster is the table's `SUB_VERBS`, in order;
//! * every flag the table says SHIPS on a sub-verb exists on that sub-verb of the command, with the
//!   arity the table claims;
//! * every flag the command declares is a row of the table (so a flag added to a struct and not to
//!   the table is a red test, not a missing line in a published page);
//! * a flag the table calls repeatable is repeatable in the command.
//!
//! ⚠ What it deliberately does NOT assert, because it is no longer true and is the integrator's to
//! replace when the table is derived: the table's `repeat: LastWins` rows. `clap` REFUSES a
//! non-repeatable flag given twice (it used to overwrite silently), so those rows now describe a
//! behaviour the binary does not have — [`the_published_last_wins_rows_are_refused_repeats_now`]
//! pins the new truth and names the rows.

use std::collections::BTreeSet;

use vike_util::cli::{FlagRow, describe};

use super::*;

use crate::surface::{FLAGS, Repeat, SUB_VERBS, Status, Value};

fn rows() -> Vec<FlagRow> {
    describe(&command())
}

/// The command's rows for one sub-verb: its OWN flags, `--help`/`--version` excluded by `describe`.
fn rows_of<'a>(all: &'a [FlagRow], sub: &str) -> Vec<&'a FlagRow> {
    all.iter().filter(|r| r.command == [sub.to_string()] && !r.positional).collect()
}

#[test]
fn the_command_lists_the_tables_sub_verbs_in_the_tables_order() {
    let names: Vec<String> = command()
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| s.get_name().to_string())
        .collect();
    assert_eq!(names, SUB_VERBS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
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
                    let takes = flag.value == Value::Required;
                    if row.takes_value != takes {
                        wrong.push(format!(
                            "{} on `{sub}`: the table says takes_value={takes}, the command {}",
                            flag.long, row.takes_value
                        ));
                    }
                    let repeats = matches!(flag.repeat, Repeat::Accumulate | Repeat::CommaListFold);
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

#[test]
fn every_flag_the_command_declares_is_a_row_of_the_table() {
    let known: BTreeSet<&str> = FLAGS.iter().map(|f| f.long.trim_start_matches("--")).collect();
    let mut unknown: Vec<String> = Vec::new();
    for row in rows().iter().filter(|r| !r.positional) {
        if !known.contains(row.name.as_str()) {
            unknown.push(format!("--{} (on `{}`)", row.name, row.command.join(" ")));
        }
    }
    assert!(
        unknown.is_empty(),
        "these flags are declared on the command and absent from `crate::surface::FLAGS`: {unknown:?}"
    );
}

/// `run` declares every row of [`crate::cmd::backtest::SUGAR`] — the table that says which profile
/// key each sugar flag lands on. A row added there without a field on the struct (or the reverse)
/// is a flag nothing reads or a key nothing can reach.
#[test]
fn the_run_command_declares_every_sugar_flag() {
    let all = rows();
    let run: BTreeSet<String> =
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

/// The table's `repeat: LastWins` rows are STALE: `clap` refuses what used to be overwritten
/// silently. Pinned here so the divergence is a fact in the tree rather than a surprise in the
/// published reference; the integrator's regenerated table replaces both.
#[test]
fn the_published_last_wins_rows_are_refused_repeats_now() {
    let refused = |argv: &[&str]| parse_run_args(argv.iter().map(|s| (*s).to_string())).is_err();
    assert!(refused(&["--profile", "a.toml", "--profile", "b.toml"]));
    assert!(refused(&["--profile", "p.toml", "--venue", "a", "--venue", "b"]));
    // …and the three flags that DO accumulate still do.
    assert!(!refused(&["--profile", "p.toml", "--param", "a=1", "--param", "b=2"]));
    assert!(!refused(&[
        "--profile",
        "p.toml",
        "--set",
        "engine.cash=1",
        "--set",
        "engine.fee_rate=2"
    ]));
    assert!(!refused(&["--profile", "p.toml", "--symbol", "A", "--symbol", "B"]));
    let stale: Vec<&str> =
        FLAGS.iter().filter(|f| f.repeat == Repeat::LastWins).map(|f| f.long).collect();
    assert!(!stale.is_empty(), "the table has no LastWins rows left — delete this test with them");
}
