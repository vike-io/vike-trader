//! The stdin control loop: newline-JSON commands and the stop words, read on their own thread.

use std::io::{BufRead, Write};
use std::sync::atomic::AtomicBool;

use vike_exec::{Command, CoreGone};
use vike_ops::stop;

/// The stdin control channel's whole behaviour, as a function of its LINES, one boolean, one
/// protocol writer (`out`: stdout in production) and two callbacks — so the rule below can be tested instead of trusted.
///
/// Reads newline-delimited input: a bare control word (`shutdown`/`quit`/`exit`/`status`/`help`) is
/// the interactive convenience; anything else is parsed as a JSON [`vike_exec::Command`] and lowered
/// through `send`. Command-decode errors go to STDERR so STDOUT stays protocol-only.
///
/// ⚠ **A command reply is an ack ONLY when `send` delivered it.** `send` returns `Err(CoreGone)` when
/// the core thread has exited and the lane is closed; the reply is then `{"kind":"error",...}` naming
/// the cause, never `{"kind":"ack"}` -- an operator who read an ack for an order the core never got
/// believes a live order exists.
///
/// ⚠ **The rule a reader gets wrong: EOF is a stop only on a TTY.** Under systemd stdin is
/// `/dev/null` and reads EOF the instant the daemon starts, so treating EOF as a stop would exit the
/// daemon at startup, every start, on every box. That is also precisely why the stdio channel could
/// never be the systemd stop path, and therefore why `vike_ops::stop` exists.
pub(super) fn control_loop(
    reader: impl BufRead,
    is_tty: bool,
    stop: &AtomicBool,
    out: &mut impl Write,
    mut send: impl FnMut(Command) -> Result<(), CoreGone>,
    mut status: impl FnMut() -> String,
) {
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("stdin read error: {e}");
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed {
            "shutdown" | "quit" | "exit" => {
                stop::request_stop(stop);
                return;
            }
            "help" => reply(
                out,
                &serde_json::json!({
                    "kind": "help",
                    "commands": "newline-JSON vike_exec::Command (e.g. {\"Order\":{\"Submit\":{…}}}); words: shutdown|quit|exit|status|help"
                })
                .to_string(),
            ),
            "status" => reply(out, &status()),
            _ => match serde_json::from_str::<Command>(trimmed) {
                Ok(cmd) => match send(cmd) {
                    Ok(()) => reply(out, &serde_json::json!({ "kind": "ack" }).to_string()),
                    Err(CoreGone) => {
                        tracing::warn!("stdin command not delivered: the core has exited");
                        reply(
                            out,
                            &serde_json::json!({
                                "kind": "error",
                                "error": "core_gone",
                                "detail": "the core thread has exited; the command was NOT delivered"
                            })
                            .to_string(),
                        );
                    }
                },
                Err(e) => {
                    // STDERR, never STDOUT: keep stdout the clean protocol surface.
                    eprintln!("vike-tradehub: not a valid JSON vike_exec::Command: {e}");
                }
            },
        }
    }
    if is_tty {
        // Ctrl-D from a human at a terminal is an explicit stop.
        stop::request_stop(stop);
    } else {
        // ⚠ On Windows this line lands on precisely the run that CANNOT be Ctrl-C'd. A non-tty
        // stdin is the background shape, and a background process has no console for a control
        // event to arrive from — so naming Ctrl-C here would send an operator to the one stop route
        // this run does not have. Name the file instead; it is the only one that works detached.
        #[cfg(windows)]
        tracing::info!(
            stop_file = %vike_ops::stop::STOP_FILE_NAME,
            "stdin control channel closed (non-tty) — the daemon keeps trading headless. Stop it \
             with Ctrl-C if this run HAS a console, or by creating the stop file named above in \
             <project>/settings/state if it does not (docs/ops/tradehub-windows.md)"
        );
        #[cfg(not(windows))]
        tracing::info!(
            "stdin control channel closed (non-tty) — the daemon keeps trading headless; stop via SIGTERM"
        );
    }
}

/// One protocol line to `out`, flushed. Best-effort: a closed stdout has no reader to inform, and a
/// reply that cannot be written must not take the control loop (and its stop words) down with it.
fn reply(out: &mut impl Write, line: &str) {
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
