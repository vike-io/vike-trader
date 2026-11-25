use super::super::{BIN, Group, refuse_a_non_group};
use super::*;
use crate::cmd::args::{self, Stop};

/// Read a `data` argv tail (everything after `data`) through the plane's real parser and resolve it
/// as `main` does: the group router, then `history`'s verb and flags, then [`args_of`]. A line that is
/// no `history` line meets the plane's own refusal, so the retired flat spellings and the unknown-group
/// answer are exercised exactly as an operator meets them.
fn parse(
    argv: impl IntoIterator<Item = String>,
    configured_address: Option<&str>,
) -> Result<Args, Stop> {
    match super::super::parse(argv)? {
        Group::History { verb } => {
            args_of(verb, configured_address).map_err(|msg| Stop::message(BIN, msg))
        }
        Group::Other(words) => Err(Stop::message(BIN, refuse_a_non_group(&words))),
        _ => Err(Stop::message(BIN, "not a `data history` line")),
    }
}

fn parse_of(args: &[&str]) -> Result<Args, Stop> {
    parse(args.iter().map(|s| s.to_string()), None)
}

/// `data history`'s clap command — what `--help` and the published reference describe.
fn hist_command() -> clap::Command {
    args::command::<super::super::Cli>(super::super::BIN)
        .find_subcommand("history")
        .expect("`data` has a `history` group")
        .clone()
}

/// `data history <verb>`'s clap command.
fn verb_command(verb: &str) -> clap::Command {
    hist_command()
        .find_subcommand(verb)
        .unwrap_or_else(|| panic!("`data history` has no `{verb}` verb"))
        .clone()
}

/// The `help` of `--flag` on `data history <verb>`, as clap holds it (the text the flag table prints).
fn flag_help(verb: &str, flag: &str) -> String {
    let cmd = verb_command(verb);
    let arg = cmd
        .get_arguments()
        .find(|a| a.get_long() == Some(flag))
        .unwrap_or_else(|| panic!("`data history {verb}` declares no --{flag}"));
    arg.get_help().unwrap_or_else(|| panic!("--{flag} has no help")).to_string()
}

#[cfg(test)]
mod grammar;

#[cfg(test)]
mod import_grammar;

#[cfg(test)]
mod write_grammar;

#[cfg(test)]
mod produced_by;

#[cfg(test)]
mod repair;

#[cfg(test)]
mod rename;

#[cfg(test)]
mod read_grammar;

#[cfg(test)]
mod list_render;

#[cfg(test)]
mod coverage_gate;

#[cfg(test)]
mod get_grammar;

#[cfg(test)]
mod fetch;
