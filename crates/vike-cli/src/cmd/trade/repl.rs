//! The REPL's loop, its per-line dispatch, and where its line history is persisted.

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::cmd::verbs::Verb;

use super::describe::{print_equity, print_orders, print_positions, print_recent, print_snapshot};
use super::grammar::{parse_repl_line, split_reason};
use super::help::print_verb_help;
use super::observe::{run_status, with_snapshot};
use super::write::run_write;
use super::{PROMPT, Session};

/// The interactive loop. Reads a line, parses it ([`parse_repl_line`]), dispatches it. Ctrl-C
/// cancels the current line (continue), Ctrl-D quits. History persists under the settings
/// directory's `state/` (best-effort).
pub(super) fn repl(session: &mut Session<'_>) -> Result<(), String> {
    let mut rl = DefaultEditor::new().map_err(|e| format!("cannot start the line editor: {e}"))?;
    let history = session.history.clone();
    if let Some(path) = &history {
        let _ = rl.load_history(path); // best-effort — a missing file is fine on first run
    }
    println!("type `help` for commands, `quit` (or Ctrl-D) to exit");

    loop {
        match rl.readline(PROMPT) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(trimmed);
                // The optional `--reason <text>` tail is split off FIRST — it is audit metadata,
                // not part of the verb grammar, so the parser never has to know about it.
                let (line, reason) = split_reason(trimmed);
                match parse_repl_line(line) {
                    Ok(Verb::Quit) => break,
                    Ok(verb) => dispatch(session, &mut rl, verb, reason),
                    Err(msg) => println!("error: {msg} (type `help`)"),
                }
            }
            // Ctrl-C: abandon this line, keep the REPL running.
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl-D / EOF: quit.
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(format!("line editor error: {e}")),
        }
    }

    if let Some(path) = &history {
        // `state/` is created lazily, on the first save — a session that never runs must not leave
        // an empty directory behind. Best-effort throughout: history is convenience, not data.
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = rl.save_history(path); // best-effort
    }
    Ok(())
}

/// Run one parsed line against the live session. Reads print an aligned table from the latest
/// snapshot; writes preview + confirm + send. `reason` is the optional `--reason` rationale, which
/// only a WRITE carries (a read has no audit record to attach it to).
fn dispatch(session: &mut Session<'_>, rl: &mut DefaultEditor, verb: Verb, reason: Option<String>) {
    // ⚠ ONE write path, for orders and node lifecycle alike. The lifecycle three used to arrive
    // here already resolved to a `WireCommand` (their own construction site) and needed an arm of
    // their own above this branch; they are `Verb`s now, so `is_write` routes all ten.
    if verb.is_write() {
        match verb.to_wire_command() {
            Some(cmd) => run_write(session, rl, cmd, reason),
            // Unreachable through the grammar (`is_write` and `to_wire_command` agree by
            // construction in [`crate::cmd::verbs`]); kept because the two are separate methods and
            // a silent no-op on a WRITE would be the worst possible way for that to break.
            None => println!("error: not a write command"),
        }
        return;
    }
    if reason.is_some() {
        println!("note: --reason is only recorded for WRITE commands; ignored here");
    }
    // Reads (and `help`).
    match verb {
        Verb::Help => print_verb_help(),
        Verb::Orders(symbol) => with_snapshot(session, |s| print_orders(s, symbol.as_deref())),
        Verb::Positions(venue) => with_snapshot(session, |s| print_positions(s, venue.as_deref())),
        Verb::Equity => with_snapshot(session, print_equity),
        Verb::Snapshot => with_snapshot(session, print_snapshot),
        Verb::Recent(n) => with_snapshot(session, |s| print_recent(s, n)),
        Verb::Status => run_status(session),
        // Writes are handled above; Quit is handled in the loop.
        _ => {}
    }
}

/// Basename of the persisted REPL history, inside the settings directory's `state/`.
///
/// PROGRAM-written, so it belongs under `state/` rather than beside the TOMLs a human edits: a
/// program rewriting a file somebody is editing loses that person's work.
const HISTORY_FILE: &str = "trade_history";

/// `<project>/settings/state/trade_history` (best-effort; `None` when no project sits above the
/// working directory, in which case history is simply not persisted).
///
/// PURE — the settings directory arrives as a parameter, resolved once by `crate::run` from the
/// same `std::env::vars()` sweep it already performs for the policy ceiling.
pub(super) fn history_file(settings_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    Some(settings_dir?.join(vike_model::paths::state_path::STATE_SUBDIR).join(HISTORY_FILE))
}
