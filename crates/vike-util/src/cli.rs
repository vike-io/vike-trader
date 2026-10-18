//! `cli` — the rules every command line of the workspace obeys, on top of `clap` (decision 0155).
//! Behind the `cli` Cargo feature of this crate (off by default), so a crate that names `vike-util`
//! for `hex` or `fnv` does not compile `clap`.
//!
//! `clap` (derive) READS the argv; this module decides only what `clap` leaves to the program, so
//! that no two programs can disagree about it:
//!
//! * [`Program`] — the ONE entry point: `Program::new(bin, version_line).parse::<Args>()` in a
//!   `main`, [`Program::try_parse_from`] in a test. It builds the program's [`clap::Command`] with
//!   the shared settings ([`Program::command`]) and routes EVERY outcome through one path:
//!   `-h`/`--help` prints the help page to STDOUT and exits 0, `-V`/`--version` prints the caller's
//!   version line (`vike_buildinfo::version_line` stays with the caller, which sits above this
//!   crate) to STDOUT and exits 0, anything else prints `<bin>: <message>` and clap's usage to
//!   STDERR and exits [`USAGE_EXIT`] (2). [`Program::try_parse_from`] returns that outcome as a
//!   [`CliExit`] so a test sees the text and the code without a process exit.
//! * **The one rule table** (decision 0155 verdict 3) and who enforces each row: `--f v` and
//!   `--f=v` are both read (`clap`); an unknown flag is refused with a "did you mean" tip (`clap`'s
//!   `suggestions`); a flag given twice is refused unless its field is a `Vec` or an
//!   `ArgAction::Append`/`Count` (`clap`'s default for any other action); a switch with a value
//!   (`--json=1`) is refused (`clap`); a BLANK value (`--f=`, `--f " "`) is refused for EVERY flag
//!   and positional by [`Program::try_parse_from`] itself, from the raw command line, so no field
//!   has to opt in ([`non_empty`] is the explicit spelling that also TRIMS); a negative number is a
//!   value (`--from -5`), because [`Program::command`] sets `allow_negative_numbers` on every
//!   value-taking argument of every subcommand; a positional beyond the declared ones is refused.
//! * **A positional is never echoed** (a secret typed as one must not reach a log): an unexpected
//!   positional, and a positional whose value fails its parser, are printed as `<hidden>` and lose
//!   `clap`'s value-carrying tips. [`Program::hide_values`] extends that to EVERY value and every
//!   unknown token that is not shaped like a flag name, for a program that reads credentials
//!   (`ctrader_authorize`). A custom value parser of such a program must not put the value in its
//!   own error text: that text is `clap`'s `source` and this module cannot scrub it. A mistyped
//!   SUBCOMMAND is echoed (with `clap`'s suggestion) unless [`Program::hide_values`] is set: the
//!   verb vocabulary is closed and the typo is what the operator needs to see.
//! * [`non_empty`] — the value parser for a field that must hold text.
//! * [`describe`] and [`commands`] — a plain-data walk of a [`clap::Command`] (no JSON: this crate
//!   has no `serde`), which `vike-cli` and `vike-docs` serialise into the published reference, so
//!   `--help`, the parser and the reference are ONE description.
//! * [`check_examples`] — parses documented example lines against a command WITHOUT running
//!   anything, so a renamed flag fails a test instead of a reader.
//!
//! What stays in each program, after the parse: cross-flag rules (a companion flag without its
//! partner, `--tick-secs > 0`), retired-variable refusals and credential reads. `clap` itself is
//! named directly at each use (`clap::Parser`) and is not re-exported from here.

use std::ffi::OsString;
use std::io::Write as _;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command, Parser};

/// The exit code of every command-line error (not `--help`/`--version`). It is what `clap` itself
/// uses; it is named here so a program's own post-parse usage refusals (a cross-flag rule) can exit
/// with the same number.
pub const USAGE_EXIT: i32 = 2;

/// What replaces a value that must not be echoed.
const HIDDEN: &str = "<hidden>";

/// `clap` only enables its `-V`/`--version` flag when the command has a version. The text it would
/// print is never used (the caller's version line is), so this is a placeholder.
const PLACEHOLDER_VERSION: &str = "0";

/// A program's identity for the parse: the name it prints in `<bin>: <message>` and in the usage
/// line, the version line `-V` prints, and whether to hide every value in a usage error.
#[derive(Clone, Copy, Debug)]
pub struct Program<'a> {
    bin: &'a str,
    version_line: &'a str,
    hide_values: bool,
}

/// How a command line ended without producing the program's arguments: the text to print, where to
/// print it and the exit code. [`CliExit::emit`] does it; a test reads the fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliExit {
    /// 0 for `--help`/`--version`, [`USAGE_EXIT`] for an error.
    pub code: i32,
    /// `true` for `--help`/`--version` (stdout), `false` for an error (stderr).
    pub to_stdout: bool,
    /// The complete text, newline-terminated.
    pub text: String,
}

impl CliExit {
    /// Print [`CliExit::text`] to its stream and exit the process with [`CliExit::code`].
    pub fn emit(&self) -> ! {
        // A closed pipe (`prog --help | head -1`) is not an error worth a different exit code.
        if self.to_stdout {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(self.text.as_bytes());
            let _ = out.flush();
        } else {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(self.text.as_bytes());
            let _ = err.flush();
        }
        std::process::exit(self.code)
    }
}

impl<'a> Program<'a> {
    /// `bin` is the name printed in `<bin>: <message>` and in the `Usage:` line; `version_line` is
    /// the whole line `-V`/`--version` prints (`vike_buildinfo::version_line(..)`).
    pub fn new(bin: &'a str, version_line: &'a str) -> Self {
        Self { bin, version_line, hide_values: false }
    }

    /// For a program that reads credentials: no VALUE of the command line appears in a usage
    /// error, only flag names (see the module doc).
    #[must_use]
    pub fn hide_values(mut self) -> Self {
        self.hide_values = true;
        self
    }

    /// `T`'s [`clap::Command`] with the settings every program shares: the binary name, a version
    /// flag on the command and each subcommand, and negative numbers readable as values.
    /// [`describe`], [`commands`] and [`check_examples`] take this command, so the reference and the
    /// parser describe the same rules.
    pub fn command<T: clap::CommandFactory>(&self) -> Command {
        shared_settings(T::command())
            .bin_name(self.bin)
            .version(PLACEHOLDER_VERSION)
            .propagate_version(true)
    }

    /// Read `args` (the command line WITHOUT the program name) as a `T`. Nothing is printed and no
    /// process exits: the outcome of `--help`, `--version` and every error is the [`CliExit`].
    pub fn try_parse_from<T, I, A>(&self, args: I) -> Result<T, CliExit>
    where
        T: Parser,
        I: IntoIterator<Item = A>,
        A: Into<OsString> + Clone,
    {
        let mut cmd = self.command::<T>();
        let argv =
            std::iter::once(OsString::from(self.bin)).chain(args.into_iter().map(Into::into));
        let mut matches = cmd.try_get_matches_from_mut(argv).map_err(|e| self.exit_for(e))?;
        refuse_blank(&mut cmd, &matches).map_err(|e| self.exit_for(e))?;
        T::from_arg_matches_mut(&mut matches).map_err(|e| self.exit_for(e.format(&mut cmd)))
    }

    /// Read the process's own command line as a `T`; on `--help`, `--version` or an error, print
    /// and exit (see [`CliExit::emit`]).
    pub fn parse<T: Parser>(&self) -> T {
        match self.try_parse_from::<T, _, _>(std::env::args_os().skip(1)) {
            Ok(parsed) => parsed,
            Err(exit) => exit.emit(),
        }
    }

    /// The single place a `clap` outcome becomes text, a stream and a code.
    fn exit_for(&self, mut err: clap::Error) -> CliExit {
        match err.kind() {
            ErrorKind::DisplayVersion => {
                CliExit { code: 0, to_stdout: true, text: format!("{}\n", self.version_line) }
            }
            ErrorKind::DisplayHelp => {
                CliExit { code: 0, to_stdout: true, text: err.render().to_string() }
            }
            // A help page printed because a required argument or subcommand is absent: `clap`
            // sends it to stderr with its own exit code, and it is not an `error:` line.
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                CliExit { code: USAGE_EXIT, to_stdout: false, text: err.render().to_string() }
            }
            _ => {
                hide_echoed_values(&mut err, self.hide_values);
                let rendered = err.render().to_string();
                let message = rendered.strip_prefix("error: ").unwrap_or(&rendered);
                CliExit {
                    code: USAGE_EXIT,
                    to_stdout: false,
                    text: format!("{}: {message}", self.bin),
                }
            }
        }
    }
}

/// The settings applied to a command AND every subcommand: a value-taking argument reads a
/// negative number as its value (`--from -5`).
///
/// ⚠ `allow_negative_numbers`, not `allow_hyphen_values`: the latter makes the NEXT token the value
/// whatever it is, so `--name --verbose` would swallow `--verbose` as `--name`'s value, where the
/// rule wants "needs a value". The cost is that `--name -foo` is refused (a single-dash token that
/// is not a number is read as a flag); `cli_tests.rs` pins both.
fn shared_settings(cmd: Command) -> Command {
    cmd.mut_args(|arg| {
        if arg.get_action().takes_values() { arg.allow_negative_numbers(true) } else { arg }
    })
    .mut_subcommands(shared_settings)
}

/// Refuse a blank value (empty or whitespace only) given on the command line, for every argument
/// of `cmd` and of the subcommand that was selected. A default is not on the command line and is
/// never refused.
fn refuse_blank(cmd: &mut Command, matches: &ArgMatches) -> Result<(), clap::Error> {
    let spellings: Vec<(String, String)> =
        cmd.get_arguments().map(|arg| (arg.get_id().to_string(), spelling(arg))).collect();
    for (id, spelled) in &spellings {
        if matches.value_source(id) != Some(ValueSource::CommandLine) {
            continue;
        }
        let Ok(Some(raw)) = matches.try_get_raw(id) else { continue };
        for value in raw {
            if value.to_string_lossy().trim().is_empty() {
                return Err(cmd.error(
                    ErrorKind::ValueValidation,
                    format!("{spelled} needs a non-empty value"),
                ));
            }
        }
    }
    if let Some((name, sub_matches)) = matches.subcommand()
        && let Some(sub) = cmd.find_subcommand_mut(name)
    {
        return refuse_blank(sub, sub_matches);
    }
    Ok(())
}

/// `--long` for a flag, `<ID>` for a positional.
fn spelling(arg: &Arg) -> String {
    match arg.get_long() {
        Some(long) => format!("--{long}"),
        None => format!("<{}>", arg.get_id()),
    }
}

/// Take the offending token out of `clap`'s context so `err.render()` cannot print it. The default
/// hides a POSITIONAL (an unexpected one, or one whose value failed its parser); `hide_all` hides
/// every value and every token that is not shaped like a flag name.
fn hide_echoed_values(err: &mut clap::Error, hide_all: bool) {
    let invalid_arg = match err.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(arg)) => Some(arg.clone()),
        _ => None,
    };
    // A flag's spec renders as `--name <NAME>`; a positional's as `[NAME]` or `<NAME>`.
    let names_a_flag = invalid_arg.as_deref().is_some_and(|arg| arg.starts_with('-'));
    match err.kind() {
        ErrorKind::UnknownArgument => {
            let Some(token) = invalid_arg else { return };
            match flag_name_of(&token, hide_all) {
                Some(name) => {
                    err.insert(ContextKind::InvalidArg, ContextValue::String(name));
                }
                None => {
                    err.insert(ContextKind::InvalidArg, ContextValue::String(HIDDEN.to_owned()));
                    err.remove(ContextKind::Suggested);
                    err.remove(ContextKind::SuggestedArg);
                    err.remove(ContextKind::SuggestedCommand);
                }
            }
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation | ErrorKind::TooManyValues => {
            if names_a_flag && !hide_all {
                return;
            }
            // An EMPTY value renders as "a value is required", which echoes nothing: keep it.
            if let Some(ContextValue::String(value)) = err.get(ContextKind::InvalidValue)
                && !value.is_empty()
            {
                err.insert(ContextKind::InvalidValue, ContextValue::String(HIDDEN.to_owned()));
            }
            err.remove(ContextKind::SuggestedValue);
        }
        ErrorKind::InvalidSubcommand if hide_all => {
            err.insert(ContextKind::InvalidSubcommand, ContextValue::String(HIDDEN.to_owned()));
            err.remove(ContextKind::SuggestedSubcommand);
            err.remove(ContextKind::SuggestedCommand);
            err.remove(ContextKind::Suggested);
        }
        _ => {}
    }
}

/// The part of an unknown token that is safe to print: the flag name without any `=value`. Without
/// `hide_all` any token starting with `-` qualifies; with it the name must also be shaped like a
/// flag (`-x`, `--some-name`), so a secret that happens to start with a dash is still hidden.
fn flag_name_of(token: &str, hide_all: bool) -> Option<String> {
    let name = token.split('=').next().unwrap_or("");
    if !name.starts_with('-') {
        return None;
    }
    let shaped =
        name.trim_start_matches('-').chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    (shaped || !hide_all).then(|| name.to_owned())
}

/// A value parser for a field that must hold text: the value is TRIMMED, and a blank one is refused
/// with `needs a non-empty value`. Use it as `#[arg(long, value_parser = vike_util::cli::non_empty)]`.
/// (Every blank value is refused by [`Program::try_parse_from`] anyway; this is the spelling that
/// also trims, and the one a `clap::Command` built by hand, outside [`Program`], needs.)
///
/// # Errors
/// The message `needs a non-empty value` when `raw` is empty or only whitespace.
pub fn non_empty(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Err("needs a non-empty value".to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

/// One argument (flag or positional) of a command, as plain data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagRow {
    /// The subcommand path from the root, root excluded; empty for the root's own arguments.
    pub command: Vec<String>,
    /// The long flag without its dashes, or a positional's id.
    pub name: String,
    pub short: Option<char>,
    pub positional: bool,
    pub takes_value: bool,
    pub required: bool,
    /// `true` for `ArgAction::Append` (a `Vec` field) and `ArgAction::Count`.
    pub repeatable: bool,
    pub default: Vec<String>,
    pub possible_values: Vec<String>,
    pub help: String,
}

/// One command (the root, then every subcommand, depth first) as plain data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRow {
    /// The subcommand path from the root; empty for the root.
    pub path: Vec<String>,
    pub about: String,
}

/// Every non-hidden argument of `cmd` and of its subcommands, depth first. Pass the command from
/// [`Program::command`]. The automatic `--help`/`--version` flags are not listed: `clap` adds them
/// only when it builds the command.
pub fn describe(cmd: &Command) -> Vec<FlagRow> {
    fn walk(cmd: &Command, path: &mut Vec<String>, rows: &mut Vec<FlagRow>) {
        for arg in cmd.get_arguments().filter(|arg| !arg.is_hide_set()) {
            let action = arg.get_action();
            rows.push(FlagRow {
                command: path.clone(),
                name: arg.get_long().map_or_else(|| arg.get_id().to_string(), str::to_owned),
                short: arg.get_short(),
                positional: arg.is_positional(),
                takes_value: action.takes_values(),
                required: arg.is_required_set(),
                repeatable: matches!(action, ArgAction::Append | ArgAction::Count),
                default: arg
                    .get_default_values()
                    .iter()
                    .map(|value| value.to_string_lossy().into_owned())
                    .collect(),
                possible_values: arg
                    .get_possible_values()
                    .iter()
                    .filter(|value| !value.is_hide_set())
                    .map(|value| value.get_name().to_owned())
                    .collect(),
                help: arg.get_help().map(ToString::to_string).unwrap_or_default(),
            });
        }
        for sub in cmd.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            path.push(sub.get_name().to_owned());
            walk(sub, path, rows);
            path.pop();
        }
    }
    let mut rows = Vec::new();
    walk(cmd, &mut Vec::new(), &mut rows);
    rows
}

/// The root and every non-hidden subcommand of `cmd`, depth first, with their one-line `about`.
pub fn commands(cmd: &Command) -> Vec<CommandRow> {
    fn walk(cmd: &Command, path: &mut Vec<String>, rows: &mut Vec<CommandRow>) {
        rows.push(CommandRow {
            path: path.clone(),
            about: cmd.get_about().map(ToString::to_string).unwrap_or_default(),
        });
        for sub in cmd.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            path.push(sub.get_name().to_owned());
            walk(sub, path, rows);
            path.pop();
        }
    }
    let mut rows = Vec::new();
    walk(cmd, &mut Vec::new(), &mut rows);
    rows
}

/// Parse each documented example line against `cmd` and report every line that `clap` (or the blank
/// rule) refuses. NOTHING is executed. Pass the command from [`Program::command`], so the lines are
/// judged by the rules the program enforces.
///
/// A line is shell-style text: the first word is the program name and must equal the command's
/// name (`bin_name`, else `name`); single quotes, double quotes and backslashes quote as in a shell;
/// a `#` at the start of a word begins a comment; an unquoted `|`, `;`, `&`, `<` or `>` ends the
/// command (a `2>&1` or a `| jq` tail is not parsed); a line ending in `\` continues on the next; a
/// leading `$ ` prompt is dropped; blank and comment-only lines are skipped. `--help` and
/// `--version` lines are accepted. A line that is not an example for this command is the CALLER's
/// to leave out: it is reported, not skipped.
///
/// # Errors
/// One message listing every refused line as `line N: <reason>: <the line>`, N counted from 1 in
/// `lines`.
pub fn check_examples(cmd: &Command, lines: &[&str]) -> Result<(), String> {
    let name = cmd.get_bin_name().unwrap_or_else(|| cmd.get_name()).to_owned();
    let mut failures = Vec::new();
    for (number, text) in logical_lines(lines) {
        let text = text.trim();
        let text = text.strip_prefix("$ ").unwrap_or(text);
        let words = match shell_words(text) {
            Ok(words) => words,
            Err(reason) => {
                failures.push(format!("line {number}: {reason}: {text}"));
                continue;
            }
        };
        let Some((program, rest)) = words.split_first() else { continue };
        if *program != name {
            failures.push(format!("line {number}: starts with `{program}`, not `{name}`: {text}"));
            continue;
        }
        let mut probe = cmd.clone();
        let argv = std::iter::once(name.clone()).chain(rest.iter().cloned());
        match probe.try_get_matches_from_mut(argv) {
            Ok(matches) => {
                if let Err(err) = refuse_blank(&mut probe, &matches) {
                    failures.push(format!("line {number}: {}: {text}", first_message_line(&err)));
                }
            }
            Err(err)
                if matches!(err.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {}
            Err(err) => {
                failures.push(format!("line {number}: {}: {text}", first_message_line(&err)))
            }
        }
    }
    if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) }
}

/// The first line of a `clap` error without its `error: ` prefix.
fn first_message_line(err: &clap::Error) -> String {
    let rendered = err.render().to_string();
    let first = rendered.lines().next().unwrap_or("");
    first.strip_prefix("error: ").unwrap_or(first).to_owned()
}

/// `lines` with backslash continuations joined, each paired with the 1-based number of its first
/// physical line. Blank lines are dropped.
fn logical_lines(lines: &[&str]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_end();
        let (number, mut text) = pending.take().unwrap_or((index + 1, String::new()));
        if let Some(stripped) = trimmed.strip_suffix('\\') {
            text.push_str(stripped);
            text.push(' ');
            pending = Some((number, text));
        } else {
            text.push_str(trimmed);
            if !text.trim().is_empty() {
                out.push((number, text));
            }
        }
    }
    if let Some((number, text)) = pending
        && !text.trim().is_empty()
    {
        out.push((number, text));
    }
    out
}

/// Split one shell-style line into words (see [`check_examples`] for the dialect).
fn shell_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return Err("unterminated single quote".to_owned()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err("unterminated double quote".to_owned()),
                        },
                        Some(inner) => current.push(inner),
                        None => return Err("unterminated double quote".to_owned()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '#' if !in_word => break,
            '|' | ';' | '&' | '<' | '>' => {
                // `2>&1`: the digit is a file descriptor, not a word.
                let fd =
                    in_word && current.len() == 1 && current.chars().all(|d| d.is_ascii_digit());
                if fd {
                    current.clear();
                    in_word = false;
                }
                break;
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            other => {
                in_word = true;
                current.push(other);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
