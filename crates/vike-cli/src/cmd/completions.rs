//! `vike-cli completions <SHELL>` — print the script that makes a shell tab-complete this binary's verbs,
//! sub-verbs and flags.
//!
//! `clap_complete` renders it from the one `clap` tree the published reference walks
//! (`crate::surface`'s `whole_command`), so a new verb or flag completes with no list kept here. Like `surface`, it
//! reads no settings, opens no store and dials nothing: the answer is a property of this BUILD. A user turns it on
//! once, for example `vike-cli completions powershell >> $PROFILE`, or
//! `vike-cli completions bash > ~/.local/share/bash-completion/completions/vike-cli`.

use std::process::ExitCode;

use clap_complete::Shell;

use crate::cmd::args;
use crate::surface::commands::whole_command;

/// The program name this verb is read under.
const BIN: &str = "vike-cli completions";

/// The parsed command line.
#[derive(clap::Parser, Debug)]
#[command(
    about = "Print the script that makes a shell tab-complete this binary's verbs and flags",
    after_help = "Turn it on once, for example:\n  vike-cli completions powershell >> $PROFILE\n  \
                  vike-cli completions bash > ~/.local/share/bash-completion/completions/vike-cli\nReads \
                  no settings, opens no store and dials nothing."
)]
struct Args {
    #[arg(value_enum, help = "the shell the script is for")]
    shell: Shell,
}

/// This verb as a `clap` command, for the published reference (`crate::surface`) and the tree it completes.
pub(crate) fn command() -> clap::Command {
    args::command::<Args>(BIN)
}

pub(crate) fn run(argv: impl Iterator<Item = String>) -> ExitCode {
    let args: Args = match args::parse(BIN, argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    write_script(args.shell, &mut std::io::stdout());
    ExitCode::SUCCESS
}

/// Write the completion script for `shell`, over the whole command tree.
fn write_script(shell: Shell, out: &mut dyn std::io::Write) {
    clap_complete::generate(shell, &mut whole_command(), args::PROGRAM, out);
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;

    use super::*;

    /// Every shell's script renders off the whole tree and names a verb and a sub-verb of it, so a tree
    /// `clap_complete` cannot walk fails here rather than on a user's shell.
    #[test]
    fn every_shell_completes_the_whole_tree() {
        for shell in Shell::value_variants() {
            let mut out = Vec::new();
            write_script(*shell, &mut out);
            let text = String::from_utf8_lossy(&out);
            assert!(text.contains("backtest") && text.contains("history"), "{shell}");
        }
    }

    #[test]
    fn the_shell_is_a_required_word_of_the_roster() {
        let parse =
            |items: &[&str]| args::parse::<Args, _, _>(BIN, items.iter().map(|s| s.to_string()));
        assert_eq!(parse(&["fish"]).expect("a shell").shell, Shell::Fish);
        assert!(parse(&["cmd"]).is_err(), "an unknown shell is refused");
        assert!(parse(&[]).is_err(), "the shell is required");
    }
}
