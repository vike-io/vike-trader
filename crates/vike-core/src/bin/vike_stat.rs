//! vike-stat — map the opt-in mmap counters file READ-ONLY and print the live core's key health
//! counters, WITHOUT attaching to the (possibly headless / remote) trader process (audit co9). The
//! trader writes the file only when `CoreConfig::counters_path` is set; this reads it. Protocol/
//! result stdout stays raw (repo logging rule for tools) — no `vike_log` init here.
//!
//!   cargo run -p vike-core --bin vike_stat -- <counters-file> [--watch] [--interval <secs>]
//!
//! The counters file is the one positional argument; no environment variable names it (decision
//! 0111). `--watch` re-reads on an interval (default 1 s); `--interval <secs>` sets the period (implies
//! `--watch`). A snapshot may be seen mid-update — the reader retries a seqlock and flags a rare
//! torn read; every counter is monotonic, so a torn value is still a valid point-in-time reading.

use std::path::PathBuf;
use std::time::Duration;

use vike_util::cli::{CliExit, Program};

/// The command line, read by `clap` through `vike_util::cli::Program` (decision 0155): the counters
/// file is the one positional; `--flag value` and `--flag=value`, an unknown flag refused with a
/// suggestion, `-h`/`--help` and `-V`/`--version` on STDOUT with exit 0 (a user pipes `--help` into
/// a pager), any other usage error on stderr with exit 2.
#[derive(clap::Parser, Debug)]
#[command(
    name = "vike_stat",
    about = "Reads the opt-in mmap counters file a live vike core mirrors (CoreConfig::counters_path)"
)]
struct Cli {
    /// The counters file.
    #[arg(value_name = "COUNTERS_FILE")]
    path: PathBuf,
    /// Re-read every 1s until interrupted.
    #[arg(long, short = 'w')]
    watch: bool,
    /// Re-read on a custom period, in seconds (implies --watch).
    #[arg(long, short = 'i', value_name = "SECS")]
    interval: Option<f64>,
}

/// What the command line asked for, resolved.
#[derive(Debug, PartialEq)]
struct Run {
    path: PathBuf,
    watch: bool,
    period: Duration,
}

/// Read the command line (WITHOUT the program name). A period under 50 ms is raised to it; an
/// `--interval` implies `--watch`.
///
/// # Errors
/// The [`CliExit`] a process would have printed: `--help`/`--version` (code 0) or a usage error
/// (code 2).
fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Run, CliExit> {
    let program = Program::new("vike_stat", concat!("vike_stat ", env!("CARGO_PKG_VERSION")));
    let Cli { path, watch, interval } = program.try_parse_from(args)?;
    let period = match interval {
        Some(secs) => Duration::from_secs_f64(secs.max(0.05)),
        None => Duration::from_secs(1),
    };
    Ok(Run { path, watch: watch || interval.is_some(), period })
}

fn main() {
    let Run { path, watch, period } = match parse_args_from(std::env::args().skip(1)) {
        Ok(run) => run,
        Err(exit) => exit.emit(),
    };

    if !watch {
        // One-shot: a read error is a non-zero exit so a script can tell "no file / bad file".
        match vike_core::counters::read(&path) {
            Ok(report) => print_report(&path, &report),
            Err(e) => {
                eprintln!("vike_stat: cannot read {}: {e}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    // Watch: re-read forever. A transient error (trader not up yet / rotating the file) is printed
    // but does NOT exit — the next tick retries.
    loop {
        match vike_core::counters::read(&path) {
            Ok(report) => print_report(&path, &report),
            Err(e) => eprintln!("vike_stat: cannot read {} (retrying): {e}", path.display()),
        }
        std::thread::sleep(period);
    }
}

fn print_report(path: &std::path::Path, r: &vike_core::CountersReport) {
    let flag = if r.clean { "clean" } else { "TORN-READ" };
    println!(
        "{}  v{} seq={} updated_ms={} [{}]",
        path.display(),
        r.version,
        r.seq,
        r.updated_ms,
        flag
    );
    for (name, value) in r.counters.named() {
        println!("  {name:<26} {value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn try_run(line: &[&str]) -> Result<Run, CliExit> {
        parse_args_from(line.iter().map(|s| (*s).to_string()))
    }

    fn run(line: &[&str]) -> (PathBuf, bool, Duration) {
        match try_run(line) {
            Ok(Run { path, watch, period }) => (path, watch, period),
            other => panic!("expected {line:?} to run, got {other:?}"),
        }
    }

    /// The refusal the test expects: a usage error on stderr with exit 2.
    fn refused(line: &[&str]) -> String {
        let exit = try_run(line).expect_err("this command line must be refused");
        assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}: {exit:?}");
        exit.text
    }

    /// POSITIVE argv pins (decision 0155, written against the hand-rolled parser and kept through
    /// the `clap` migration): the positional path with and without each flag, long and short.
    #[test]
    fn the_documented_lines_are_accepted_with_their_meaning() {
        let (path, watch, period) = run(&["/run/vike/counters.bin"]);
        assert_eq!(path, PathBuf::from("/run/vike/counters.bin"));
        assert!(!watch);
        assert_eq!(period, Duration::from_secs(1));

        let (_, watch, period) = run(&["counters.bin", "--watch"]);
        assert!(watch);
        assert_eq!(period, Duration::from_secs(1));

        // `--interval` implies `--watch`, takes fractional seconds, and sits before or after the path.
        let (path, watch, period) = run(&["--interval", "2.5", "counters.bin"]);
        assert_eq!(path, PathBuf::from("counters.bin"));
        assert!(watch);
        assert_eq!(period, Duration::from_secs_f64(2.5));
        let (_, watch, period) = run(&["counters.bin", "--interval", "3"]);
        assert!(watch);
        assert_eq!(period, Duration::from_secs(3));
    }

    #[test]
    fn the_short_flags_are_accepted() {
        let (_, watch, _) = run(&["-w", "counters.bin"]);
        assert!(watch);
        let (_, watch, period) = run(&["counters.bin", "-i", "4"]);
        assert!(watch);
        assert_eq!(period, Duration::from_secs(4));
        let (_, watch, period) = run(&["-w", "-i", "0.5", "counters.bin"]);
        assert!(watch);
        assert_eq!(period, Duration::from_secs_f64(0.5));
    }

    /// A period under the floor is raised to it, not refused.
    #[test]
    fn a_period_under_the_floor_is_raised_to_it() {
        let (_, _, period) = run(&["counters.bin", "--interval", "0"]);
        assert_eq!(period, Duration::from_secs_f64(0.05));
        let (_, _, period) = run(&["counters.bin", "-i", "0.001"]);
        assert_eq!(period, Duration::from_secs_f64(0.05));
        // A negative number is a VALUE (decision 0155), raised to the floor like any short one.
        let (_, _, period) = run(&["counters.bin", "--interval", "-1"]);
        assert_eq!(period, Duration::from_secs_f64(0.05));
    }

    /// `--flag=value` is read like `--flag value`.
    #[test]
    fn the_equals_spelling_is_read() {
        let (_, watch, period) = run(&["counters.bin", "--interval=2"]);
        assert!(watch);
        assert_eq!(period, Duration::from_secs(2));
    }

    /// `-h`/`--help` and `-V`/`--version` succeed on STDOUT (a user pipes `--help` into a pager).
    #[test]
    fn help_and_version_are_a_success_on_stdout() {
        for flag in ["-h", "--help"] {
            let exit = try_run(&[flag]).expect_err("help is an exit");
            assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}");
            assert!(exit.text.contains("--interval"), "{}", exit.text);
        }
        let exit = try_run(&["--version"]).expect_err("version is an exit");
        assert_eq!(exit.text, concat!("vike_stat ", env!("CARGO_PKG_VERSION"), "\n"));
    }

    #[test]
    fn no_path_a_second_path_and_a_bad_interval_are_refused() {
        refused(&[]);
        refused(&["a", "b"]);
        refused(&["a", "--interval"]);
        let text = refused(&["a", "--interval", "x"]);
        assert!(text.contains("--interval"), "{text}");
        refused(&["a", "--nope"]);
    }
}
