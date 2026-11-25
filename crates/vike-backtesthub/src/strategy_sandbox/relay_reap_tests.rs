//! The REAP of a sandboxed run's child ([`super::run`]): an answered run is answered before its child exits and the
//! child is reaped on a thread of its own, which the next run on the same thread waits for; a failed run still reaps
//! at once and names the exit. Through real processes: stand-in children written as shell scripts.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use vike_backtest::run::StrategySource;
use vike_backtesthub_client::proto::{Request, Response};
use vike_data::HistoryStoreMemory;

use vike_hub_protocol::child_pipe::{send_build, write_message_named};

use super::super::RustRun;
use super::super::wire::ChildFrame;
use super::{Outcome, run, wait_for_the_last_reap};

/// The build both ends state here (the binary hands in `vike_buildinfo::summary`; a library links none).
const BUILD: &str = "relay_reap_tests";

/// A stand-in child `dir/<name>` that writes its pid to `dir/<name>.pid`, then either states this build, answers
/// `Pong` and execs `then` (so the process that answered is the one that lingers or exits) or exits 3 having answered
/// nothing.
fn stand_in(dir: &Path, name: &str, then: Option<&str>) -> PathBuf {
    let pid = dir.join(format!("{name}.pid")).display().to_string();
    let script = match then {
        Some(then) => {
            let answer = dir.join(format!("{name}.answer"));
            let mut frame = Vec::new();
            send_build(&mut frame, BUILD).expect("its build");
            write_message_named(&mut frame, &ChildFrame::Answer(Box::new(Response::Pong)))
                .expect("the answer frame");
            std::fs::write(&answer, frame).expect("the answer");
            let answer = answer.display();
            format!("#!/bin/sh\necho $$ > {pid}\n/bin/cat {answer}\nexec {then}\n")
        }
        None => format!("#!/bin/sh\necho $$ > {pid}\nexit 3\n"),
    };
    let path = dir.join(name);
    std::fs::write(&path, script).expect("the script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    path
}

/// `program`'s run of a `Ping`, again while the script just written is busy in a fork of another test's thread
/// (`ETXTBSY`), and the pid it wrote.
fn run_retrying_etxtbsy(dir: &Path, program: &Path) -> (Outcome, u32) {
    // The stand-ins read no argument, so the artifact the builder would name is left empty.
    let runs = [RustRun {
        source: StrategySource::of(String::new()),
        artifact: String::new(),
        models: BTreeMap::new(),
    }];
    let store = HistoryStoreMemory::new();
    for _ in 0..20 {
        let outcome = run(program, BUILD, &Request::Ping, dir, &runs, &store, None, None, None);
        if !matches!(&outcome, Outcome::Failed(why) if why.contains("os error 26")) {
            let pid = std::fs::read_to_string(program.with_extension("pid")).expect("its pid");
            return (outcome, pid.trim().parse().expect("a pid"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{} stayed busy", program.display());
}

/// The state letter of process `pid` (`/proc/<pid>/stat`'s third field), `None` once it is reaped.
fn state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.trim_start().chars().next()
}

/// An answered run comes back while its child still runs (it lingers past `EXIT_GRACE`, so it is killed and reaped
/// by its reaper); the next run on this thread starts only once that child is reaped, so no zombie is left and a
/// thread never has two children alive; a child that fails is reaped before its run answers, its exit named.
#[test]
fn an_answered_run_does_not_wait_for_its_child_and_the_next_run_waits_for_its_reap() {
    let dir = tempfile::tempdir().expect("a folder for the stand-ins");
    let lingering = stand_in(dir.path(), "lingering", Some("/bin/sleep 5"));
    // Exits when the parent closes its channel, as a real child that read its request: one exiting at once (`sleep
    // 0`) closed the pipe before a loaded parent wrote the request, failing the run on a broken pipe.
    let prompt = stand_in(dir.path(), "prompt", Some("/bin/cat >/dev/null"));
    let failing = stand_in(dir.path(), "failing", None);

    let (outcome, first) = run_retrying_etxtbsy(dir.path(), &lingering);
    assert!(matches!(&outcome, Outcome::Answer(a) if matches!(a.response, Response::Pong)));
    assert!(
        state(first).is_some_and(|s| s != 'Z'),
        "answered while the child still runs: {:?}",
        state(first)
    );

    let (outcome, second) = run_retrying_etxtbsy(dir.path(), &prompt);
    if let Outcome::Failed(why) = &outcome {
        panic!("the prompt child's run failed: {why}");
    }
    assert!(matches!(&outcome, Outcome::Answer(_)));
    assert_eq!(state(first), None, "the next run started after the first child was reaped");

    let (outcome, third) = run_retrying_etxtbsy(dir.path(), &failing);
    let Outcome::Failed(why) = outcome else {
        panic!("a child that answers nothing fails its run")
    };
    assert!(why.contains("exit status: 3"), "a failed run names its child's exit: {why}");
    assert_eq!(state(third), None, "a failed run's child is reaped before it answers");
    assert_eq!(state(second), None, "and the answered one before it, no zombie left");
    wait_for_the_last_reap();
}
