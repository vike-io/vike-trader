//! `docs_data` — write the `docs-data` release assets this crate renders (`rendered_files`'s set:
//! `venues.json`, `stats.json`, `indicators.json`, `templates.json`, `rosters.json`; `bins.json`
//! is xtask's and `cli.json` is the CLI's own, each rendered by its own step beside this one
//! because neither is derivable from a compile-time table THIS crate can see — xtask's comes from
//! `cargo metadata`, and `cli.json`'s table is compiled into `vike-cli`, which declares a layer
//! above this crate's (30 against 25) and so may not be depended on from here).
//!
//! A thin file-writing shell over `vike_docs::rendered_files`, which is where the whole
//! rendering lives (so `crates/vike-docs/tests/docs_data_gate.rs` gates the exact bytes this bin
//! writes, in-process, without spawning anything). Consumed by `.github/workflows/release.yml`'s
//! "Render the docs-data assets" step, which runs it into `target/release` so every file joins
//! `SHA256SUMS` and the upload loop beside every other asset.
//!
//! Usage: `docs_data [OUT_DIR] [GENERATED_FROM]` — two optional positional arguments: the output
//! directory (default `.`), and the commit `stats.json` reports as `generated_from` (default
//! `vike_docs::DEFAULT_GENERATED_FROM`; a blank value is refused rather than defaulted).
//! No environment reads, no settings, no network: the inputs are compile-time tables plus that one
//! argument, and the output is one file per entry of that set.
//!
//! The commit is an argument, not a compile-time bake (`vike_docs::DEFAULT_GENERATED_FROM`'s doc
//! carries why); a binary reading its own argv is the sanctioned shape.

use std::path::Path;
use std::process::ExitCode;

use vike_docs::{generated_from, rendered_files};
use vike_util::cli::Program;

/// The command line, read by `clap` through `vike_util::cli::Program` (decision 0155): two optional
/// positionals, `-h`/`--help` and `-V`/`--version` on stdout with exit 0, any usage error (a third
/// positional, an unknown flag, a blank value) on stderr with exit 2.
#[derive(clap::Parser, Debug, PartialEq, Eq)]
#[command(
    name = "docs_data",
    about = "Write the docs-data release assets the vike-docs crate renders"
)]
struct Cli {
    /// The directory the files are written into.
    #[arg(value_name = "OUT_DIR", default_value = ".")]
    out_dir: String,
    /// The commit stats.json reports as generated_from (default: vike_docs::DEFAULT_GENERATED_FROM).
    #[arg(value_name = "GENERATED_FROM")]
    generated_from: Option<String>,
}

/// The program's identity for the parse.
fn program() -> Program<'static> {
    Program::new("docs_data", concat!("docs_data ", env!("CARGO_PKG_VERSION")))
}

fn main() -> ExitCode {
    let Cli { out_dir, generated_from: stamp_arg } = program().parse();
    let stamp = match generated_from(stamp_arg.as_deref()) {
        Ok(v) => v,
        Err(why) => {
            eprintln!("docs_data: {why}");
            eprintln!("usage: docs_data [OUT_DIR] [GENERATED_FROM]");
            return ExitCode::from(2);
        }
    };
    for (name, contents) in rendered_files(stamp) {
        let path = Path::new(&out_dir).join(name);
        if let Err(e) = std::fs::write(&path, &contents) {
            eprintln!("docs_data: writing {} failed: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {} ({} bytes)", path.display(), contents.len());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Result<Cli, vike_util::cli::CliExit> {
        program().try_parse_from::<Cli, _, _>(line.iter().copied())
    }

    fn cli(out_dir: &str, generated_from: Option<&str>) -> Cli {
        Cli { out_dir: out_dir.to_string(), generated_from: generated_from.map(str::to_string) }
    }

    /// POSITIVE argv pins (decision 0155, written against the hand-rolled reader and kept through
    /// the `clap` migration): the no-argument form, `release.yml`'s two positionals
    /// (`docs_data target/release "$(git rev-parse HEAD)"`) and the one-positional form.
    #[test]
    fn the_documented_lines_are_accepted_with_their_meaning() {
        assert_eq!(parse(&[]).unwrap(), cli(".", None));
        assert_eq!(parse(&["target/release"]).unwrap(), cli("target/release", None));
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(parse(&["target/release", sha]).unwrap(), cli("target/release", Some(sha)));
    }

    #[test]
    fn a_third_positional_and_an_unknown_flag_are_usage_errors() {
        for line in [&["out", "abc", "extra"][..], &["--nope"]] {
            let exit = parse(line).expect_err("this line must be refused");
            assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}: {exit:?}");
        }
    }

    #[test]
    fn help_and_version_are_a_success_on_stdout() {
        let exit = parse(&["--help"]).expect_err("help is an exit");
        assert_eq!((exit.code, exit.to_stdout), (0, true));
        assert!(exit.text.contains("GENERATED_FROM"), "{}", exit.text);
        let exit = parse(&["-V"]).expect_err("version is an exit");
        assert_eq!(exit.text, concat!("docs_data ", env!("CARGO_PKG_VERSION"), "\n"));
    }
}
