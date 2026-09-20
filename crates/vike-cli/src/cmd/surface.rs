//! `vike-cli surface` — write this binary's own command surface as JSON.
//!
//! The thin writer over [`crate::surface::rendered_files`]. The TABLE and the rendering live in
//! that module; this one only chooses where the bytes land, so a release workflow and a developer
//! at a prompt reach identical output.
//!
//! # Why a subcommand rather than `--help --json`
//!
//! Help travels back through the `Err` channel as `crate::cmd::args`'s `HELP_SENTINEL`, and
//! `exit_for_parse_error` prints the static usage on stdout at exit 0. **That path carries no
//! payload.** A JSON variant would need a second sentinel, and any message not byte-equal to a
//! sentinel falls through to the `eprintln!` + `Exit::Usage` arm — so a near-miss silently becomes
//! a usage error, which is the failure that function's own doc records having already happened once.
//!
//! Not an `xtask` either, and not a second `[[bin]]`: `crates/vike-cli/Cargo.toml` declares no
//! `[[bin]]` section, `crates/vike-ops/tests/packaging_gate.rs` polices that manifest, and a new bin
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

use crate::cmd::args::{self, Flags, exit_for_parse_error};
use crate::exit::Exit;
use crate::surface::rendered_files;

/// ⚠ **Written FLUSH LEFT because that is how it renders.**
///
/// Every line of a `\`-continued Rust literal swallows the newline AND all leading whitespace of
/// the next line, so source indentation in a const like this one is invisible at runtime — it only
/// misleads the next person editing it. Indentation that must SURVIVE is spelled with explicit
/// spaces after the `\n`, as the two option rows below do.
pub(crate) const USAGE: &str = "usage: vike-cli surface [--out DIR] [--list]\n\
\n\
\x20 --out DIR    write each asset into DIR, creating it if needed\n\
\x20 --list       print the asset names one per line and write nothing\n\
\n\
With neither flag the assets are printed to stdout, one after another.\n\
Reads no settings, opens no store and dials nothing — the table is compiled in.";

/// The parsed command line.
#[derive(Debug, Default)]
struct Args {
    /// Where the assets land. `None` prints them instead.
    out: Option<String>,
    /// Print the names and write nothing.
    list: bool,
}

/// Hand-rolled, over the shared [`crate::cmd::args`] glue — the same shape every other verb uses,
/// so `--flag value` and `--flag=value` both work and `-h` short-circuits through the `Err` channel.
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut flags = Flags::new(argv);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--out" => a.out = Some(flags.value(&flag, inline)?),
            "--list" => {
                args::no_value(&flag, inline)?;
                a.list = true;
            }
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(a)
}

pub fn run(argv: impl Iterator<Item = String>) -> ExitCode {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("surface", USAGE, &msg),
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
