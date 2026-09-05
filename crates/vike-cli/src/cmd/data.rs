//! `vike-cli data` — get market data into the hist store, without knowing a second binary's name.
//!
//! ```text
//! vike-cli data fetch <VENUE:SYMBOL:INTERVAL> (--days N | --from LABEL --to LABEL) [--store DIR]
//! vike-cli data seed-demo [--store DIR]
//! ```
//!
//! # What this verb is, and what it deliberately is not
//!
//! It is **not a collector**. Every byte of the work is done by the standalone `backtest` engine,
//! which has carried `--fetch`, `--seed-demo` and their siblings since long before this verb
//! existed. What was missing was DISCOVERABILITY: `vike-cli init` ends by telling a new user to run
//! `backtest --seed-demo`, which is a different binary with a different name, and a person who
//! installed "the vike CLI" has no reason to expect that the tool that runs a backtest is not the
//! tool that fetches the data for one. So this is a route, and [`crate::cmd::engine`] is the ONE
//! place that knows where the engine lives and what its exit codes mean.
//!
//! ⚠ **It could not be anything else.** Fetching writes a hist store, which needs DataFusion, and
//! this crate's whole identity is being DataFusion-free — argued edge by edge in
//! `crates/vike-cli/Cargo.toml` and machine-checked by CI's `light-consumers` lane. `engine`'s
//! module doc carries the full argument for spawning rather than linking; it applies here
//! unchanged.
//!
//! # What is validated HERE, and why only that much
//!
//! The spec's SHAPE (`VENUE:SYMBOL:INTERVAL`, three non-empty parts) and the WINDOW (`--days N`, or
//! `--from`/`--to` together, exactly one of the two forms). Both are things an operator gets wrong
//! by typing, and catching them here makes them a usage error instead of a process spawn whose
//! diagnostic arrives from a binary the user did not name.
//!
//! What is NOT validated here is the VENUE and the INTERVAL, deliberately: which venues a build can
//! reach is a property of the ENGINE's `venue-fetch` feature and its collectors, and a roster copied
//! into this crate would be a second list to keep in step — refusing a venue the engine supports, or
//! accepting one it does not, with equal confidence. The engine's own error names what it can do.

use std::path::Path;
use std::process::ExitCode;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested};
use crate::cmd::engine;
use crate::exit::CmdResult;

const USAGE: &str = "\
usage: vike-cli data <subcommand> [options]

Get market data into the hist store the backtest engine reads. Every subcommand drives the
standalone `backtest` engine — attached beside this binary in a Linux release, and on Windows
a binary you supply yourself (the failure message says how) — see --engine below.

subcommands:
  fetch SPEC   pull REAL public bars into the store. SPEC is VENUE:SYMBOL:INTERVAL
               (e.g. binance:BTCUSDT:1h). Needs a window: --days, or --from/--to.
               No credentials — this is public market data
  seed-demo    write the SYNTHETIC demo tape into the store. Venue `demo`, a closed-form
               curve, NOT market data — it is the slice the shipped
               user_data/profiles/backtest.toml names, so a fresh install can run that
               profile immediately. Safe to re-run

options:
  --days N        fetch: a window counting back from now
  --from LABEL    fetch: window start — epoch-ms, or YYYY-MM-DDTHH
  --to LABEL      fetch: window end, same spellings
  --store DIR     the hist-store root to write into
  --engine PATH   the standalone engine to run, instead of searching <project>/bin,
                  this executable's directory, and PATH
  -h, --help      this message";

/// Which subcommand ran. Two, and adding a third is one arm here plus one row in [`USAGE`].
#[derive(Debug, PartialEq, Eq)]
enum Sub {
    /// `fetch SPEC` — real public bars, over a window.
    Fetch,
    /// `seed-demo` — the synthetic tape, no window and no network.
    SeedDemo,
}

/// The window a `fetch` covers. Exactly one form, chosen by the operator; there is no default,
/// because "fetch everything" is not a thing any venue serves and a silent default would decide how
/// much of somebody's rate limit to spend.
#[derive(Debug, PartialEq, Eq)]
enum Window {
    /// `--days N`, counting back from now.
    Days(String),
    /// `--from LABEL --to LABEL`, an explicit range.
    Range { from: String, to: String },
}

/// The parsed `data` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    /// `VENUE:SYMBOL:INTERVAL`, shape-checked. `None` for `seed-demo`.
    spec: Option<String>,
    /// `None` for `seed-demo`; always `Some` for `fetch` (the parser refuses one without).
    window: Option<Window>,
    store: Option<String>,
    engine: Option<String>,
}

/// Parse `data`'s own argv tail (everything after the verb). PURE — no I/O, no spawn.
fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err("a subcommand is required (fetch | seed-demo)".to_string());
    };
    let sub = match first.as_str() {
        "fetch" => Sub::Fetch,
        "seed-demo" => Sub::SeedDemo,
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `data` subcommand '{other}'")),
    };

    let mut spec: Option<String> = None;
    let mut days: Option<String> = None;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut store: Option<String> = None;
    let mut engine: Option<String> = None;

    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--days" => days = Some(flags.value(&flag, inline)?),
            "--from" => from = Some(flags.value(&flag, inline)?),
            "--to" => to = Some(flags.value(&flag, inline)?),
            "--store" => store = Some(flags.value(&flag, inline)?),
            "--engine" => engine = Some(flags.value(&flag, inline)?),
            "-h" | "--help" => return help_requested(),
            // ⚠ Judged on the `--` prefix, the same rule `crate::cmd::args`'s `is_flag_token`
            // spells for every valued flag in this crate: a token beginning with `--` is a FLAG,
            // so an unrecognised one is a usage error rather than something to read as a spec.
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            // The one POSITIONAL in this verb: the spec. Anything after the first is a mistake
            // worth naming — a second bare word is nearly always a shell-quoting accident, and
            // silently ignoring it would fetch a series the operator did not ask for.
            positional => match &spec {
                None => spec = Some(positional.to_string()),
                Some(already) => {
                    return Err(format!(
                        "unexpected extra argument '{positional}' (the spec is already \
                         '{already}'); one series per fetch"
                    ));
                }
            },
        }
    }

    match sub {
        Sub::Fetch => {
            let spec = spec.ok_or(
                "fetch needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data fetch \
                 binance:BTCUSDT:1h --days 180`",
            )?;
            check_spec(&spec)?;
            let window = window_from(days, from, to)?;
            Ok(Args { sub, spec: Some(spec), window: Some(window), store, engine })
        }
        Sub::SeedDemo => {
            // Every fetch-shaped flag is REFUSED here rather than ignored. The demo tape is a
            // closed-form curve computed over a fixed span: a `--days 30` that quietly did nothing
            // would leave an operator believing they had seeded a month.
            if spec.is_some() {
                return Err("seed-demo takes no spec — it writes the synthetic `demo` tape".into());
            }
            for (flag, present) in
                [("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())]
            {
                if present {
                    return Err(format!(
                        "{flag} applies to `fetch` only — the demo tape is a fixed synthetic span"
                    ));
                }
            }
            Ok(Args { sub, spec: None, window: None, store, engine })
        }
    }
}

/// The spec's SHAPE: three non-empty `:`-separated parts. See this module's doc for why the venue
/// and interval themselves are the ENGINE's to judge.
fn check_spec(spec: &str) -> Result<(), String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{spec}' is not VENUE:SYMBOL:INTERVAL — three non-empty parts, e.g. binance:BTCUSDT:1h"
        ));
    }
    Ok(())
}

/// The window, as exactly one of the two forms.
///
/// ⚠ Mixing them is an error rather than a precedence rule. `--days 30 --from 2026-01-01T00` has
/// two readable meanings and no obviously right one, and whichever a precedence rule picked would
/// silently discard the other half of what the operator typed.
fn window_from(
    days: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<Window, String> {
    match (days, from, to) {
        (Some(d), None, None) => {
            let n: u32 = d
                .trim()
                .parse()
                .map_err(|_| format!("--days takes a whole number of days, got {d:?}"))?;
            if n == 0 {
                return Err("--days 0 covers no time at all".to_string());
            }
            Ok(Window::Days(d))
        }
        (None, Some(f), Some(t)) => Ok(Window::Range { from: f, to: t }),
        (None, Some(_), None) => Err("--from needs a matching --to".to_string()),
        (None, None, Some(_)) => Err("--to needs a matching --from".to_string()),
        (None, None, None) => Err(
            "fetch needs a window: --days N, or --from LABEL --to LABEL (epoch-ms or YYYY-MM-DDTHH)"
                .to_string(),
        ),
        (Some(_), _, _) => {
            Err("--days and --from/--to are two ways to say the same thing — pass one".to_string())
        }
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `data` verb; `project_root`
/// is `<project>`, resolved once by [`crate::run`], and is how the engine under `<project>/bin` is
/// found — a PARAMETER, because a `src/cmd/` file may not read the environment for itself.
pub fn run(args: impl Iterator<Item = String>, project_root: Option<&Path>) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("data", USAGE, &msg),
    };
    match execute(&args, project_root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli data: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Build the child's argv and hand it to [`crate::cmd::engine`]. Nothing else happens in this
/// module: the store is opened, written and reported on by the engine, whose streams this process
/// inherits.
fn execute(args: &Args, project_root: Option<&Path>) -> CmdResult<()> {
    let program = engine::locate(args.engine.as_deref(), project_root);
    engine::run(&program, &engine_argv(args), "data")
}

/// The engine flags this verb's arguments become — PURE, so the translation is unit-tested rather
/// than only observed through a spawn.
fn engine_argv(args: &Args) -> Vec<String> {
    let mut argv = Vec::new();
    match args.sub {
        Sub::Fetch => {
            argv.push("--fetch".to_string());
            // Present by construction: `parse` refuses a `fetch` without one.
            argv.push(args.spec.clone().unwrap_or_default());
            match &args.window {
                Some(Window::Days(d)) => {
                    argv.push("--days".to_string());
                    argv.push(d.clone());
                }
                Some(Window::Range { from, to }) => {
                    argv.push("--from".to_string());
                    argv.push(from.clone());
                    argv.push("--to".to_string());
                    argv.push(to.clone());
                }
                None => {}
            }
        }
        Sub::SeedDemo => argv.push("--seed-demo".to_string()),
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args::HELP_SENTINEL;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn each_subcommand_parses() {
        assert_eq!(
            parse_of(&["fetch", "binance:BTCUSDT:1h", "--days", "7"]).unwrap().sub,
            Sub::Fetch
        );
        assert_eq!(parse_of(&["seed-demo"]).unwrap().sub, Sub::SeedDemo);
    }

    #[test]
    fn help_short_circuits_at_both_levels() {
        assert_eq!(parse_of(&["--help"]).unwrap_err(), HELP_SENTINEL);
        assert_eq!(parse_of(&["fetch", "-h"]).unwrap_err(), HELP_SENTINEL);
    }

    #[test]
    fn a_missing_subcommand_names_the_two_that_exist() {
        let err = parse_of(&[]).unwrap_err();
        assert!(err.contains("fetch") && err.contains("seed-demo"), "{err}");
    }

    /// ⚠ The shape check is a TYPO catcher, not a roster. Three non-empty parts pass whatever they
    /// name; two, four, or an empty part do not.
    #[test]
    fn the_spec_shape_is_checked_and_nothing_else_is() {
        assert!(check_spec("binance:BTCUSDT:1h").is_ok());
        assert!(check_spec("notavenue:WHATEVER:3q").is_ok(), "the engine judges the venue, not us");
        for bad in ["binance:BTCUSDT", "a:b:c:d", "binance::1h", ":BTCUSDT:1h", "binance:BTCUSDT:"]
        {
            let err = check_spec(bad).unwrap_err();
            assert!(err.contains(bad), "the message names what was typed: {err}");
            assert!(err.contains("VENUE:SYMBOL:INTERVAL"), "…and the shape it wanted: {err}");
        }
    }

    /// A fetch with no window is a usage error naming both spellings — the engine would refuse it
    /// too, but only after a process spawn and in a binary the user did not name.
    #[test]
    fn a_fetch_needs_a_window() {
        let err = parse_of(&["fetch", "binance:BTCUSDT:1h"]).unwrap_err();
        assert!(err.contains("--days"), "{err}");
        assert!(err.contains("--from"), "{err}");
    }

    /// The two window forms are EXCLUSIVE, and a half-range is named for the half that is missing.
    #[test]
    fn the_window_forms_do_not_mix() {
        assert!(window_from(Some("7".into()), Some("a".into()), Some("b".into())).is_err());
        assert!(window_from(None, Some("a".into()), None).unwrap_err().contains("--to"));
        assert!(window_from(None, None, Some("b".into())).unwrap_err().contains("--from"));
        assert_eq!(
            window_from(None, Some("a".into()), Some("b".into())).unwrap(),
            Window::Range { from: "a".into(), to: "b".into() }
        );
    }

    /// `--days` is a whole positive number: a `--days 7.5` or a `--days 0` is caught here rather
    /// than becoming a fetch that covers nothing.
    #[test]
    fn days_must_be_a_positive_whole_number() {
        assert!(window_from(Some("7".into()), None, None).is_ok());
        assert!(window_from(Some("7.5".into()), None, None).is_err());
        assert!(window_from(Some("-3".into()), None, None).is_err());
        assert!(window_from(Some("0".into()), None, None).unwrap_err().contains("no time"));
    }

    /// `seed-demo` refuses every fetch-shaped flag rather than ignoring it — a `--days 30` that
    /// quietly did nothing would leave an operator believing they had seeded a month.
    #[test]
    fn seed_demo_refuses_a_window_and_a_spec() {
        assert!(parse_of(&["seed-demo", "--days", "30"]).unwrap_err().contains("--days"));
        assert!(parse_of(&["seed-demo", "--from", "0"]).unwrap_err().contains("--from"));
        assert!(parse_of(&["seed-demo", "binance:BTCUSDT:1h"]).unwrap_err().contains("no spec"));
        // …and the one flag that DOES apply to it still parses.
        assert_eq!(parse_of(&["seed-demo", "--store", "/s"]).unwrap().store.as_deref(), Some("/s"));
    }

    /// A second bare word is a shell-quoting accident far more often than an intention, and
    /// ignoring it would fetch a series nobody asked for.
    #[test]
    fn a_second_positional_is_refused_naming_both() {
        let err = parse_of(&["fetch", "binance:BTCUSDT:1h", "okx:BTC-USDT:1h", "--days", "7"])
            .unwrap_err();
        assert!(err.contains("okx:BTC-USDT:1h") && err.contains("binance:BTCUSDT:1h"), "{err}");
    }

    /// THE translation: what the engine is actually asked to do. Pinned as argv because that is
    /// the whole product of this module — everything else is the engine's.
    #[test]
    fn the_engine_argv_is_the_translation() {
        let days =
            parse_of(&["fetch", "binance:BTCUSDT:1h", "--days", "180", "--store", "/s"]).unwrap();
        assert_eq!(
            engine_argv(&days),
            ["--fetch", "binance:BTCUSDT:1h", "--days", "180", "--store", "/s"]
        );

        let range = parse_of(&["fetch", "okx:BTC-USDT:1h", "--from", "0", "--to", "100"]).unwrap();
        assert_eq!(
            engine_argv(&range),
            ["--fetch", "okx:BTC-USDT:1h", "--from", "0", "--to", "100"]
        );

        assert_eq!(engine_argv(&parse_of(&["seed-demo"]).unwrap()), ["--seed-demo"]);
        assert_eq!(
            engine_argv(&parse_of(&["seed-demo", "--store", "/s"]).unwrap()),
            ["--seed-demo", "--store", "/s"]
        );
    }

    /// `--engine` never reaches the child: it says WHICH binary to run, not what to tell it.
    #[test]
    fn the_engine_flag_is_consumed_here_and_not_forwarded() {
        let a = parse_of(&["seed-demo", "--engine", "/opt/backtest"]).unwrap();
        assert_eq!(a.engine.as_deref(), Some("/opt/backtest"));
        assert!(!engine_argv(&a).iter().any(|s| s == "--engine"), "{:?}", engine_argv(&a));
    }
}
