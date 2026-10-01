//! **DOES THE JAR ITSELF SAY ANYTHING?** — spawn the JForex sidecar DIRECTLY, with both streams on
//! the terminal, and watch.
//!
//!     cargo test -p vike-dukascopy --test dukascopy_jar_probe -- --ignored --nocapture
//!
//! # ⚠ What the login smoke cannot see, and why that matters
//!
//! `DukascopyExecutionClient::spawn` gives the child `stderr(Stdio::inherit())` and
//! `stdout(Stdio::piped())`. JForex's own logging goes to STDERR — which is why the smoke's output
//! is full of `AuthorizationClient` and `PingHelper` lines — while the sidecar's own protocol, the
//! `ready` / `fatal` envelopes, goes to STDOUT, straight into a Rust reader thread.
//!
//! So a run that times out has TWO readings that the smoke cannot tell apart:
//!
//! * the jar never got a session, and stdout carried nothing;
//! * the jar DID hand back `ready` and the Rust side failed to parse or route it.
//!
//! Measured on the CI box 2026-09-21, both demo accounts stalled after
//! `ClientConnector - Authorize task in queue` with 230s of silence on stderr — and stdout was
//! never visible in either run. This probe makes it visible: both streams are INHERITED, so every
//! byte the jar produces lands on the terminal in the order it was written.
//!
//! # ⚠ It is a PROBE and it asserts nothing
//!
//! It places no order, and it fails no build: the question is *what does the jar say*, and an
//! assertion would only turn an answer into a verdict. Read the output.
//!
//! Credentials come from the store exactly as the smoke takes them — they are handed to the child
//! through the ENVIRONMENT and never printed, never put in argv.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_dukascopy::{DukascopyAccount, load_dukascopy_config_from};

/// How long to watch before giving up — deliberately LONGER than the client's own `READY_TIMEOUT`
/// (300s), so a session that merely took longer than the smoke allows shows up as a late `ready`
/// rather than as the same silent timeout.
const WATCH: Duration = Duration::from_secs(420);

/// `$VIKE_SETTINGS_DIR`, the one fact this probe pulls out of the process environment by name.
fn settings_dir_override() -> Option<String> {
    std::env::var("VIKE_SETTINGS_DIR").ok().filter(|s| !s.trim().is_empty())
}

#[test]
#[ignore = "live: network + demo creds + the sidecar jar — a MEASUREMENT, run manually"]
fn does_the_jar_itself_say_anything() {
    vike_log::test_init();

    // DUKASCOPY_SMOKE_ACCOUNT=demo2 switches accounts, same spelling the login smoke uses.
    let account = if std::env::var("DUKASCOPY_SMOKE_ACCOUNT").as_deref() == Ok("demo2") {
        DukascopyAccount::Demo2
    } else {
        DukascopyAccount::Demo1
    };
    let vars = load_workspace_dotenv_from(settings_dir_override().as_deref());
    let Some(cfg) = load_dukascopy_config_from(account, &vars) else {
        println!("SKIP: no credentials for {account:?} in this box's store");
        return;
    };

    let jar = std::env::var("JFOREX_BRIDGE_JAR")
        .expect("point JFOREX_BRIDGE_JAR at the sidecar jar for this probe");
    let java = std::env::var("JAVA_HOME")
        .map(|h| format!("{h}/bin/java"))
        .unwrap_or_else(|_| "java".to_string());
    // The SAME JNLP the client resolves when `server` is blank — the sidecar's own default, and
    // the one both accounts were measured against.
    let jnlp = "https://www.dukascopy.com/client/demo/jclient/jforex.jnlp";

    println!("=== spawning the jar DIRECTLY: {java} -jar {jar}");
    println!("=== jnlp: {jnlp}");
    println!(
        "=== account: {account:?}  (login and password go through the ENVIRONMENT, unprinted)"
    );

    let mut child = Command::new(&java)
        .arg("-jar")
        .arg(&jar)
        .env("DUKASCOPY_LOGIN", &cfg.login)
        .env("DUKASCOPY_PASSWORD", &cfg.password)
        .env("DUKASCOPY_JNLP", jnlp)
        // ⚠ BOTH inherited — that is the whole point. The smoke pipes stdout into a Rust reader,
        // so the sidecar's own protocol has never been seen by a human on this box.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the sidecar");

    // stdout is read HERE, line by line, and echoed with a stamp — so a `ready` that arrives late
    // is distinguishable from one that never arrives, which the smoke's timeout cannot do.
    let mut out = child.stdout.take().expect("piped stdout");
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut acc = String::new();
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            acc.push_str(&String::from_utf8_lossy(&buf[..n]));
            while let Some(i) = acc.find('\n') {
                let line: String = acc.drain(..=i).collect();
                if tx.send(line.trim_end().to_string()).is_err() {
                    return;
                }
            }
        }
    });

    let mut saw_anything = false;
    loop {
        let left = WATCH.checked_sub(started.elapsed());
        let Some(left) = left else { break };
        match rx.recv_timeout(left.min(Duration::from_secs(15))) {
            Ok(line) => {
                saw_anything = true;
                println!("[stdout +{:>4}s] {line}", started.elapsed().as_secs());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if started.elapsed() >= WATCH {
                    break;
                }
                println!("[  ...   +{:>4}s] nothing on stdout yet", started.elapsed().as_secs());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                println!("[stdout +{:>4}s] CLOSED", started.elapsed().as_secs());
                break;
            }
        }
    }

    println!(
        "=== watched {}s; the jar {} anything on stdout",
        started.elapsed().as_secs(),
        if saw_anything { "DID say" } else { "said NOTHING" }
    );
    let _ = child.kill();
    let _ = child.wait();
}
