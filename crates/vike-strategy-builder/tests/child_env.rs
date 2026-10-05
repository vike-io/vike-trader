//! The builder's `cargo` child receives the ALLOWLIST and nothing else — proven against the real
//! daemon, over the real socket, by reading the environment the child was actually exec'd with.
//!
//! ⚠ **Why this exists.** `cargo` runs a caller's `build.rs` and proc-macros, so whatever
//! environment reaches cargo reaches arbitrary code (`crates/vike-strategy-builder/src/builder.rs`'s
//! Decision 2). MEASURED on the production unit on 2026-09-26: the spawn in
//! `crates/vike-strategy-builder/src/render.rs`'s `build_plugin` cleared nothing, so the service's
//! own key rode into every build as an ordinary environment variable. The fix is `env_clear()` plus
//! `render::CHILD_ENV_ALLOWLIST`; this is the behavioural proof, and deleting that `env_clear()`
//! call is the mutation it goes red on.
//!
//! ⚠ **Why a spawned daemon and a planted `cargo`, not a call to `render::child_env`.** A unit test
//! of the allowlist function stays green with the `env_clear()` deleted — the function would still
//! compute the right list, and the spawn would still inherit everything else on top of it. The
//! property is about what the CHILD PROCESS receives, so the only honest witness is the child: the
//! planted `cargo` copies its own `/proc/<pid>/environ` next to the manifest it was handed and exits
//! non-zero, which the service reports back as a compile failure carrying the stub's stderr.
//!
//! The daemon inherits this test runner's WHOLE environment (dozens of `CARGO_*`/`RUST*` names a
//! test process carries) plus a planted secret-shaped canary, so "nothing outside the allowlist
//! reached the child" is asserted against a crowd, not against an empty set. Two positive controls
//! keep it from passing vacuously: `TMPDIR` must arrive carrying the value the daemon was given, and
//! so must `PATH`.
//!
//! Linux only: the capture reads `/proc`. Cheap and un-ignored — nothing is compiled.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use vike_node_proto::auth::NodeKeys;
use vike_strategy_builder::builder::{BUILDER_KEY_ENV, BUILDER_KEY_FILE_ENV, WORKSPACE_ROOT_ENV};
use vike_strategy_builder::client::{BuildRequestError, build_remote};
use vike_strategy_builder::render::CHILD_ENV_ALLOWLIST;

/// The argument the planted `cargo` answers with an immediate exit 0 — ONE spelling for the probe
/// and the stub's guard, for the reason `crates/vike-agent-eval/tests/claude_cli.rs`'s own
/// `SETTLE_ARG` gives: two literals that drifted apart would make the settle probe fall through to
/// the stub's real work and report the file settled for the wrong reason.
const SETTLE_ARG: &str = "--vike-settle";

/// The file the planted `cargo` writes its environment to, beside the manifest it was handed.
const CAPTURE: &str = "child.environ";

/// The daemon, killed however the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Exec a freshly planted script once, retrying ONLY `ETXTBSY`, so the DAEMON's exec of it cannot
/// lose that race — a local copy of `crates/vike-agent-eval/tests/claude_cli.rs`'s
/// `settle_planted_exec` (the mechanism is argued there and in `crates/vike-cli/tests/common/mod.rs`;
/// this crate takes no `vike-ops` dependency for a test helper). The race is live here: this binary
/// runs its two cases as parallel threads, and each spawns a daemon while the other may be planting.
fn settle_planted_exec(path: &Path) {
    const ETXTBSY: i32 = 26;
    const ATTEMPTS: u32 = 8;
    let mut backoff = Duration::from_millis(5);
    for attempt in 1..=ATTEMPTS {
        match Command::new(path).arg(SETTLE_ARG).output() {
            Ok(out) if out.status.success() => return,
            Ok(out) => panic!(
                "settle {}: the stub RAN and exited {:?} — it did not answer `{SETTLE_ARG}` with \
                 its guard. stderr: {}",
                path.display(),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) if e.raw_os_error() == Some(ETXTBSY) => {}
            Err(e) => panic!(
                "settle {}: could not be spawned and it is NOT the ETXTBSY race — errno {:?}: {e}",
                path.display(),
                e.raw_os_error()
            ),
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(backoff);
            backoff *= 2;
        }
    }
    panic!("settle {}: still ETXTBSY after {ATTEMPTS} attempts", path.display());
}

/// A `cargo` that records the environment it was exec'd with and fails. `$$` is the shell itself,
/// so `/proc/$$/environ` is exactly the block the builder handed `execve` — not a shell's re-export
/// of it — and `/bin/cat` is named outright because the whole point is that `PATH` may not arrive.
fn plant_fake_cargo(dir: &Path) -> PathBuf {
    let path = dir.join("fake-cargo");
    let script = format!(
        "#!/bin/sh\n\
         [ \"$1\" = {SETTLE_ARG} ] && exit 0\n\
         manifest=''\n\
         prev=''\n\
         for a in \"$@\"; do\n\
         \x20 if [ \"$prev\" = --manifest-path ]; then manifest=\"$a\"; fi\n\
         \x20 prev=\"$a\"\n\
         done\n\
         [ -n \"$manifest\" ] || {{ echo 'fake cargo: no --manifest-path' >&2; exit 3; }}\n\
         /bin/cat /proc/$$/environ > \"${{manifest%/Cargo.toml}}/{CAPTURE}\"\n\
         echo 'fake cargo: environment captured' >&2\n\
         exit 1\n"
    );
    std::fs::write(&path, script).expect("write the fake cargo");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the fake cargo executable");
    settle_planted_exec(&path);
    path
}

fn stderr_lines(child: &mut Child) -> Receiver<String> {
    let stderr = child.stderr.take().expect("stderr was piped");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn wait_for_banner(rx: &Receiver<String>) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) if line.contains("listening on") => {
                return line
                    .split("listening on ")
                    .nth(1)
                    .and_then(|rest| rest.split(',').next())
                    .map(|a| a.trim().to_string())
                    .expect("the banner names an address");
            }
            Ok(line) => seen.push(line),
            Err(_) => panic!("the daemon never printed its listening banner: {seen:#?}"),
        }
    }
}

/// Every file named [`CAPTURE`] under `dir`.
fn captures_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == CAPTURE) {
                out.push(p);
            }
        }
    }
    out
}

/// How the daemon is handed its key in one run.
enum KeyForm {
    Value,
    File,
}

/// Start the daemon with `form`, ask it for one Build, and return the environment its `cargo`
/// child received as `(name, value)` pairs — plus the key and every planted value, for the caller
/// to look for.
fn child_environment(form: KeyForm) -> (Vec<(String, String)>, Planted) {
    let work = tempfile::tempdir().expect("tempdir");
    let root = tempfile::tempdir().expect("workspace root (unstamped, so the build reaches cargo)");
    let out = tempfile::tempdir().expect("out dir");
    let tmp = tempfile::tempdir().expect("the daemon's TMPDIR, where its scratch crate lands");
    let cargo = plant_fake_cargo(work.path());

    // A secret-shaped canary, ASSEMBLED at run time so no source file in this tree spells it whole
    // (the settings registry reads an env-shaped literal as "this crate reads that variable").
    let canary_name = ["CANARY", "VENUE", "API", "SECRET"].join("_");
    let canary_value = "canary-value-that-must-never-reach-a-build-script".to_string();
    let key = "child-env-test-key-not-a-real-credential".to_string();
    let key_file = work.path().join("builder-key");
    std::fs::write(&key_file, format!("{key}\n")).expect("write the key file");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-strategy-builder"));
    match form {
        KeyForm::Value => {
            cmd.env(BUILDER_KEY_ENV, &key).env_remove(BUILDER_KEY_FILE_ENV);
        }
        KeyForm::File => {
            cmd.env(BUILDER_KEY_FILE_ENV, &key_file).env_remove(BUILDER_KEY_ENV);
        }
    }
    let mut child = cmd
        .env(WORKSPACE_ROOT_ENV, root.path())
        .env("VIKE_STRATEGY_BUILDER_PORT", "0")
        .env("VIKE_STRATEGY_BUILDER_OUT_DIR", out.path())
        .env("CARGO", &cargo)
        .env("TMPDIR", tmp.path())
        .env(&canary_name, &canary_value)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the builder binary");
    let rx = stderr_lines(&mut child);
    let _daemon = Daemon(child);
    let addr = wait_for_banner(&rx);

    let keys = NodeKeys::new(Vec::new(), key.as_bytes().to_vec());
    match build_remote(&addr, &keys, "child_env_probe", "pub fn untouched() {}", "") {
        Err(BuildRequestError::Compile(diagnostics)) => assert!(
            diagnostics.contains("fake cargo: environment captured"),
            "the build failed, but not in the planted cargo — so it never ran and this test would \
             prove nothing: {diagnostics}"
        ),
        other => panic!("expected the planted cargo's failure as a BuildErr, got {other:?}"),
    }

    let captures = captures_under(tmp.path());
    assert_eq!(
        captures.len(),
        1,
        "expected exactly one captured environment under the daemon's TMPDIR, found {captures:?}"
    );
    let raw = std::fs::read(&captures[0]).expect("read the captured environment");
    let pairs = raw
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            match entry.split_once('=') {
                Some((n, v)) => (n.to_string(), v.to_string()),
                None => (entry.to_string(), String::new()),
            }
        })
        .collect();
    let planted = Planted {
        key,
        key_file: key_file.display().to_string(),
        canary_name,
        canary_value,
        tmpdir: tmp.path().display().to_string(),
    };
    (pairs, planted)
}

/// What one run planted in the daemon's environment.
struct Planted {
    key: String,
    key_file: String,
    canary_name: String,
    canary_value: String,
    tmpdir: String,
}

fn assert_only_the_allowlist_reached_cargo(pairs: &[(String, String)], planted: &Planted) {
    let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
    for forbidden in [BUILDER_KEY_ENV, BUILDER_KEY_FILE_ENV, planted.canary_name.as_str()] {
        assert!(
            !names.contains(&forbidden),
            "`{forbidden}` reached the cargo child — and through it every `build.rs`. Names the \
             child got: {names:?}"
        );
    }
    for (name, value) in pairs {
        assert!(
            !value.contains(&planted.key),
            "the service KEY reached the cargo child as the value of `{name}` (length {} — the \
             value is not printed)",
            planted.key.len()
        );
        assert!(
            !value.contains(&planted.canary_value) && !value.contains(&planted.key_file),
            "a planted value reached the cargo child through `{name}`"
        );
    }
    let allowed: Vec<&str> = CHILD_ENV_ALLOWLIST.iter().map(|(n, _)| *n).collect();
    let stray: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !allowed.contains(n) && *n != "CARGO_TARGET_DIR")
        .collect();
    assert!(
        stray.is_empty(),
        "the cargo child received names outside `render::CHILD_ENV_ALLOWLIST` (plus the \
         `CARGO_TARGET_DIR` the spawn sets itself): {stray:?}. The spawn must start from \
         `env_clear()`."
    );
    // The positive controls: an EMPTY environment would satisfy every assertion above.
    let get = |n: &str| pairs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    assert_eq!(
        get("TMPDIR"),
        Some(planted.tmpdir.as_str()),
        "TMPDIR is on the allowlist and the daemon was given one, so the child must receive it"
    );
    assert!(get("PATH").is_some_and(|p| !p.is_empty()), "PATH is on the allowlist and must arrive");
}

#[test]
fn a_planted_secret_and_the_value_form_key_never_reach_the_cargo_child() {
    let (pairs, planted) = child_environment(KeyForm::Value);
    assert_only_the_allowlist_reached_cargo(&pairs, &planted);
}

#[test]
fn the_key_file_path_never_reaches_the_cargo_child_either() {
    let (pairs, planted) = child_environment(KeyForm::File);
    assert_only_the_allowlist_reached_cargo(&pairs, &planted);
}
