//! `vike-cli surface` — write this binary's own command surface as JSON.
//!
//! The thin writer over [`crate::surface::rendered_files`]. The TABLE and the rendering live in
//! that module; this one only chooses where the bytes land, so a release workflow and a developer
//! at a prompt reach identical output.
//!
//! # Why a subcommand rather than `--help --json`
//!
//! `--help` is `clap`'s (`crate::cmd::args::Stop`): it carries the help page and nothing else, so
//! a JSON variant of it would have had no way to travel back through the parse. A subcommand's
//! own output has no such limit.
//!
//! Not an `xtask` either, and not a second `[[bin]]`: `crates/vike-cli/Cargo.toml` declares no
//! `[[bin]]` section, `crates/vike-ops/tests/release/packaging_gate.rs` polices that manifest, and a new bin
//! would join the release matrix for one JSON file.
//!
//! # What it deliberately does not do
//!
//! It reads no settings, opens no store, dials no daemon and touches no credential. The table is
//! compiled in, so **the answer is a property of this BUILD** — which is what makes it safe to run
//! inside a release workflow, and what lets the gate call `rendered_files` in-process instead of
//! spawning this verb.

use std::path::Path;
use std::process::ExitCode;

use crate::cmd::args::{self, Stop};
use crate::exit::Exit;
use crate::surface::rendered_files;

/// The program name this verb is read under.
const BIN: &str = "vike-cli surface";

/// The parsed command line.
#[derive(clap::Parser, Debug, Default)]
#[command(
    about = "Write this binary's own command surface as JSON",
    after_help = "With neither flag the assets are printed to stdout, one after another.\nReads no \
                  settings, opens no store and dials nothing — the table is compiled in."
)]
struct Args {
    /// Where the assets land. `None` prints them instead.
    #[arg(long, value_name = "DIR", help = "write each asset into DIR, creating it if needed")]
    out: Option<String>,
    /// Print the names and write nothing.
    #[arg(long, help = "print the asset names one per line and write nothing")]
    list: bool,
}

fn parse(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    args::parse(BIN, argv)
}

pub fn run(argv: impl Iterator<Item = String>) -> ExitCode {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    let files = rendered_files();

    if args.list {
        for name in files.keys() {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }

    let Some(dir) = args.out.as_deref() else {
        for body in files.values() {
            print!("{body}");
        }
        return ExitCode::SUCCESS;
    };

    let dir = Path::new(dir);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("vike-cli surface: cannot create {}: {e}", dir.display());
        return Exit::Failed.into();
    }
    for (name, body) in files.iter() {
        let path = dir.join(name);
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("vike-cli surface: cannot write {}: {e}", path.display());
            return Exit::Failed.into();
        }
        eprintln!("wrote {}", path.display());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> impl Iterator<Item = String> {
        items.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn an_empty_line_prints_to_stdout() {
        let a = parse(argv(&[])).expect("no flags is a valid line");
        assert!(a.out.is_none());
        assert!(!a.list);
    }

    #[test]
    fn out_takes_a_value_in_both_spellings() {
        assert_eq!(
            parse(argv(&["--out", "dist"])).expect("space form").out.as_deref(),
            Some("dist")
        );
        assert_eq!(parse(argv(&["--out=dist"])).expect("equals form").out.as_deref(), Some("dist"));
    }

    /// `--list` is a bare switch, so the inline spelling is a usage error rather than a directory.
    #[test]
    fn list_is_a_bare_switch() {
        assert!(parse(argv(&["--list"])).expect("bare form").list);
        assert!(parse(argv(&["--list=1"])).is_err(), "--list takes no value");
    }

    #[test]
    fn an_unknown_option_is_refused_by_name() {
        let err = parse(argv(&["--nope"])).expect_err("unknown options are refused");
        assert!(err.contains("--nope"), "the refusal names the flag: {err}");
    }

    /// ⚠ The verb must be able to answer with NO project, NO store and NO network, because a release
    /// workflow runs it in a bare checkout. Asserted by calling the renderer directly: if it ever
    /// grew an I/O dependency, this test would be the first thing to fail.
    #[test]
    fn the_assets_render_without_any_environment() {
        let files = rendered_files();
        assert!(!files.is_empty(), "at least one asset is published");
        for (name, body) in files.iter() {
            assert!(!name.is_empty(), "an asset carries a name");
            assert!(!body.is_empty(), "{name} rendered empty");
        }
    }
}
