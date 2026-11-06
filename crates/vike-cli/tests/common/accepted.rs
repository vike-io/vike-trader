//! The harness behind the POSITIVE-argv pin files (`tests/argv_accepted_*.rs`, decision 0155).
//!
//! A pin file lists command lines an operator, a deployed unit or a runbook could write and have
//! ACCEPTED, and asks one question of each: **did the shipped `vike-cli` get past its argument
//! reader?** The answer is read off the exit code — `Exit::Usage` (2) is what a command line the
//! reader refuses answers (before and after the move to `clap`), and every other outcome (0, a
//! failed run, an unreachable daemon, a refused LOCAL precondition) means the line was understood
//! and the verb went on to do something about it.
//!
//! Each line runs in a SANDBOX: the working directory is a fresh temp dir, `VIKE_SETTINGS_DIR` and
//! `VIKE_USER_DATA_DIR` point inside it, the risk and hub-key variables are cleared, stdin is
//! closed (a REPL or a stdin-fed verb meets end-of-file at once) and the child is killed after
//! [`LIMIT`]. A case therefore touches nothing of the machine it runs on, dials only loopback
//! ports nothing listens on, and writes only into the sandbox.
//!
//! The pin files were written against the hand-rolled parsers and run UNCHANGED against the
//! `clap` ones: a line that stops being accepted is a regression of the operator's command line,
//! whatever the new wording of its refusal says.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one command line may run before it is killed. Generous: every case should end in a
/// refused dial or a refused precondition within a second.
const LIMIT: Duration = Duration::from_secs(30);

/// What a line may end in. `Not2` is the pin: the argument reader accepted the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Any exit code but the usage rung (2). The default for a line whose later outcome depends on
    /// the sandbox (a refused dial, a missing file).
    Not2,
    /// Exactly this exit code — for a line whose whole outcome is certain (`--help`, a pure read).
    Code(i32),
}

/// One pinned line: the arguments after `vike-cli`, what it must end in, and why it is here.
#[derive(Debug, Clone, Copy)]
pub struct Pin {
    pub argv: &'static [&'static str],
    pub expect: Expect,
    /// Where an operator meets this line (a unit's `ExecStart=`, a runbook, the README, a verb's
    /// own doc) — the reason it must keep working.
    pub why: &'static str,
}

/// A pin that only asks "not a usage error".
pub const fn pin(argv: &'static [&'static str], why: &'static str) -> Pin {
    Pin { argv, expect: Expect::Not2, why }
}

/// A pin that asks for an exact exit code.
pub const fn pin_code(argv: &'static [&'static str], code: i32, why: &'static str) -> Pin {
    Pin { argv, expect: Expect::Code(code), why }
}

/// Run every pin in its own sandbox and fail with ONE message naming every line that was refused.
pub fn assert_accepted(pins: &[Pin]) {
    let mut failures = Vec::new();
    for p in pins {
        let (code, stderr) = run_in_sandbox(p.argv);
        let ok = match p.expect {
            Expect::Not2 => code != Some(2),
            Expect::Code(want) => code == Some(want),
        };
        if !ok {
            failures.push(format!(
                "  vike-cli {}\n    expected {:?}, got {:?} ({})\n    stderr: {}",
                p.argv.join(" "),
                p.expect,
                code,
                p.why,
                stderr.trim().lines().take(6).collect::<Vec<_>>().join(" | ")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} pinned command line(s) were not accepted:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Spawn the shipped binary on `argv` inside a fresh sandbox; the exit code (`None` when killed by
/// a signal or by the time limit) and the captured stderr.
pub fn run_in_sandbox(argv: &[&str]) -> (Option<i32>, String) {
    let sandbox = tempfile::tempdir().expect("a scratch directory");
    let settings = sandbox.path().join("settings");
    let user_data = sandbox.path().join("user_data");
    std::fs::create_dir_all(&settings).expect("a scratch settings directory");
    std::fs::create_dir_all(&user_data).expect("a scratch user_data directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(argv)
        .current_dir(sandbox.path())
        .env("VIKE_SETTINGS_DIR", &settings)
        .env("VIKE_USER_DATA_DIR", &user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        .env_remove("VIKE_DATAHUB_OBSERVE_KEY")
        .env_remove("VIKE_DATAHUB_CONTROL_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn vike-cli {argv:?}: {e}"));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > LIMIT => {
                let _ = child.kill();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("wait for vike-cli {argv:?}: {e}"),
        }
    }
    let out = child.wait_with_output().unwrap_or_else(|e| panic!("collect vike-cli {argv:?}: {e}"));
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}
