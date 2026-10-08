//! The stdin control loop: newline-JSON commands and the stop words, read on their own thread.

use std::io::{BufRead, Write};
use std::sync::atomic::AtomicBool;

use vike_exec::Command;
use vike_ops::stop;

/// The stdin control channel's whole behaviour, as a function of its LINES, one boolean, and two
/// callbacks — so the rule below can be tested instead of trusted.
///
/// Reads newline-delimited input: a bare control word (`shutdown`/`quit`/`exit`/`status`/`help`) is
/// the interactive convenience; anything else is parsed as a JSON [`vike_exec::Command`] and lowered
/// through `send`. Command-decode errors go to STDERR so STDOUT stays protocol-only.
///
/// ⚠ **The rule a reader gets wrong: EOF is a stop only on a TTY.** Under systemd stdin is
/// `/dev/null` and reads EOF the instant the daemon starts, so treating EOF as a stop would exit the
/// daemon at startup, every start, on every box. That is also precisely why the stdio channel could
/// never be the systemd stop path, and therefore why `vike_ops::stop` exists.
pub(super) fn control_loop(
    reader: impl BufRead,
    is_tty: bool,
    stop: &AtomicBool,
    mut send: impl FnMut(Command),
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
            "help" => {
                println!(
                    "{}",
                    serde_json::json!({
                        "kind": "help",
                        "commands": "newline-JSON vike_exec::Command (e.g. {\"Order\":{\"Submit\":{…}}}); words: shutdown|quit|exit|status|help"
                    })
                );
                let _ = std::io::stdout().flush();
            }
            "status" => {
                println!("{}", status());
                let _ = std::io::stdout().flush();
            }
            _ => match serde_json::from_str::<Command>(trimmed) {
                Ok(cmd) => {
                    send(cmd);
                    println!("{}", serde_json::json!({ "kind": "ack" }));
                    let _ = std::io::stdout().flush();
                }
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
