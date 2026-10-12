//! `secrets ibc-start` and `secrets ibkr-cp-login`, asserted over the SHIPPED binary: the gateway's
//! login is taken from the settings database (so a rotation through `secrets set` is the pair the
//! next launch uses), the child receives it on its ARGV (IBC) or in its ENVIRONMENT (Client Portal)
//! and nowhere else, no value reaches any stream, the state directory or the project tree, and every
//! refusal names the repair and runs nothing — the first of them the SESSION check (decision 0140):
//! each verb logs in only for its own `(product, tier)` pair, the one the settings database states.
//!
//! ⚠ No credential NAME is spelled as one literal: they are composed by [`name`], because an
//! env-shaped literal in this tree is read by the settings registry's sweep as a read, and the
//! planted VALUES are obvious sentinels — nothing here is, or resembles, a real login.
//!
//! The "child" is a fake `ibcstart.sh` / `run-login.sh` planted where the verb looks for it. It runs
//! through `bash` (no exec bit needed) and writes what it was given to a file beside itself.

use std::path::{Path, PathBuf};

use super::support::{SetCase, exit_code, rung};
use super::{BIN, stderr, stdout};

const USER: &str = "sentinel-login-id-7f3a";
const PASS: &str = "sentinel-pass-4b2d9c";
const LIVE_USER: &str = "sentinel-LIVE-id-81aa";
const LIVE_PASS: &str = "sentinel-LIVE-pass-c0de";

fn name(tier: &str, field: &str) -> String {
    format!("IBKR_{tier}_{field}")
}

fn ok(o: &std::process::Output) {
    assert_eq!(exit_code(o), rung(vike_cli::exit::Exit::Ok), "{}\n{}", stdout(o), stderr(o));
}

fn failed(o: &std::process::Output) -> String {
    assert_eq!(
        exit_code(o),
        rung(vike_cli::exit::Exit::Failed),
        "expected a refusal on the run-failure rung:\n{}\n{}",
        stdout(o),
        stderr(o)
    );
    format!("{}{}", stdout(o), stderr(o))
}

fn put(c: &SetCase, key: &str, value: &str) {
    ok(&c.run(&["set", key], Some(&format!("{value}\n")), &[]));
}

/// A refusal of the session pair check: the `refused` rung (nothing left this machine).
fn refused(o: &std::process::Output) -> String {
    assert_eq!(
        exit_code(o),
        rung(vike_cli::exit::Exit::Refused),
        "expected a session refusal on the refused rung:\n{}\n{}",
        stdout(o),
        stderr(o)
    );
    format!("{}{}", stdout(o), stderr(o))
}

/// `vike-cli config set KEY VALUE` against this case's settings directory — how an operator writes
/// a `venue.ibkr.<tier>.backend` row.
fn config_set(c: &SetCase, key: &str, value: &str) {
    let out = std::process::Command::new(BIN)
        .args(["config", "set", key, value])
        .env("VIKE_SETTINGS_DIR", c.settings())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    ok(&out);
}

/// [`project`] on the Client Portal product: `venue.ibkr.demo.backend = cpapi`.
fn cp_project(tag: &str) -> SetCase {
    let c = project(tag);
    config_set(&c, "venue.ibkr.demo.backend", "cpapi");
    c
}

/// The id of the ibkr account row at `tier`, out of the shipped binary's own `accounts` listing
/// (`  id  venue  tier  …`).
fn ibkr_row(c: &SetCase, tier: &str) -> String {
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    listed
        .lines()
        .filter(|l| l.starts_with(' '))
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|t| t.len() > 2 && t[1] == "ibkr" && t[2] == tier && t[0].parse::<i64>().is_ok())
        .map(|t| t[0].to_string())
        .unwrap_or_else(|| panic!("no ibkr {tier} account row in the listing: {listed}"))
}

fn deactivate(c: &SetCase, id: &str) {
    ok(&c.run(&["account", "deactivate", "--id", id], None, &[]));
}

/// A project with a settings database that holds the DEMO pair, put in the way an operator puts one
/// in: the empty store created by `secrets init`, each value on stdin to `secrets set`.
fn project(tag: &str) -> SetCase {
    let c = SetCase::new(tag);
    ok(&c.run(&["init"], None, &[]));
    put(&c, &name("DEMO", "USERNAME"), USER);
    put(&c, &name("DEMO", "PASSWORD"), PASS);
    c
}

/// The IB Gateway install the verb is pointed at: `<project>/bin/ibkr-gateway`.
fn gw_root(c: &SetCase) -> PathBuf {
    c.dir().join("bin").join("ibkr-gateway")
}

/// A fake `ibcstart.sh` plus the files the verb checks for. It records its argv, its environment,
/// its own and its parent's command line, then exits with `$DRIVER_EXIT`.
const FAKE_IBCSTART: &str = "#!/usr/bin/env bash\n\
    here=\"$(cd \"$(dirname \"$0\")/../..\" && pwd)\"\n\
    {\n\
      printf 'argv:%s\\n' \"$@\"\n\
      env | sed 's/^/env:/'\n\
      if [ -r /proc/$$/cmdline ]; then printf 'self-argv=%s\\n' \"$(tr '\\0' ' ' < /proc/$$/cmdline)\"; fi\n\
      if [ -r /proc/$PPID/cmdline ]; then printf 'parent-argv=%s\\n' \"$(tr '\\0' ' ' < /proc/$PPID/cmdline)\"; fi\n\
    } > \"$here/probe.out\"\n\
    exit ${DRIVER_EXIT:-0}\n";

fn install_gateway(c: &SetCase) -> PathBuf {
    let root = gw_root(c);
    std::fs::create_dir_all(root.join("ibc").join("scripts")).unwrap();
    std::fs::create_dir_all(root.join("jts")).unwrap();
    std::fs::write(root.join("ibc").join("scripts").join("ibcstart.sh"), FAKE_IBCSTART).unwrap();
    std::fs::write(root.join("ibc").join("config.ini"), "IbLoginId=\nIbPassword=\n").unwrap();
    root.join("probe.out")
}

fn ibc_start(c: &SetCase, envs: &[(&str, &str)]) -> std::process::Output {
    ibc_start_tier(c, None, envs)
}

/// `ibc-start`, with `--tier` when `tier` is given (the plain call omits it, as every pre-existing
/// caller and `start-gateway.sh` do).
fn ibc_start_tier(c: &SetCase, tier: Option<&str>, envs: &[(&str, &str)]) -> std::process::Output {
    let root = gw_root(c);
    let root = root.to_str().expect("utf-8");
    let mut args = vec!["ibc-start"];
    if let Some(t) = tier {
        args.extend(["--tier", t]);
    }
    args.extend(["--root", root, "--gateway-version", "1045", "--java-path", "/jre/bin"]);
    c.run(&args, None, envs)
}

/// The LIVE gateway's own files beside the demo's (`install_gateway` first): its IBC ini and its
/// own settings directory. The probe is the same file the fake `ibcstart.sh` writes.
fn install_live_gateway(c: &SetCase) -> PathBuf {
    let probe = install_gateway(c);
    let root = gw_root(c);
    std::fs::write(root.join("ibc").join("config-live.ini"), "IbLoginId=\nIbPassword=\n").unwrap();
    std::fs::create_dir_all(root.join("jts-live")).unwrap();
    probe
}

/// A project holding ONLY the LIVE pair, put in the way an operator puts one in.
fn live_project(tag: &str) -> SetCase {
    let c = SetCase::new(tag);
    ok(&c.run(&["init"], None, &[]));
    put(&c, &name("LIVE", "USERNAME"), LIVE_USER);
    put(&c, &name("LIVE", "PASSWORD"), LIVE_PASS);
    c
}

/// Every file under `<settings>/state`, concatenated — the ledger and anything else a verb wrote.
fn state_text(c: &SetCase) -> String {
    tree_text(&c.settings().join("state"), &[])
}

/// Every file under `root` (bytes read lossily), skipping any path whose name is in `skip`.
fn tree_text(root: &Path, skip: &[&str]) -> String {
    let mut out = String::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| skip.iter().any(|s| n == *s)) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(t) = std::fs::read(&p) {
                out.push_str(&String::from_utf8_lossy(&t));
            }
        }
    }
    out
}

#[test]
fn the_child_receives_the_pair_on_its_argv_and_nowhere_else() {
    let c = project("ibc-argv");
    let probe = install_gateway(&c);
    let ran = ibc_start(&c, &[]);
    ok(&ran);

    let seen = std::fs::read_to_string(&probe).expect("the launcher ran and wrote its probe");
    let root = gw_root(&c);
    for want in [
        format!("argv:--user={USER}\n"),
        format!("argv:--pw={PASS}\n"),
        "argv:--mode=paper\n".to_string(),
        "argv:--gateway\n".to_string(),
        "argv:1045\n".to_string(),
        "argv:--java-path=/jre/bin\n".to_string(),
        format!("argv:--ibc-ini={}\n", root.join("ibc").join("config.ini").display()),
        format!("argv:--ibc-path={}\n", root.join("ibc").display()),
        format!("argv:--tws-path={}\n", root.join("jts").display()),
        // The demo gateway's settings directory IS the install directory, as it always was.
        format!("argv:--tws-settings-path={}\n", root.join("jts").display()),
    ] {
        assert!(seen.contains(&want), "the launcher was not handed {want:?}: {seen}");
    }
    // …and ONLY there. Every line that is not an `argv:` word, or the launcher's own command line
    // (which is those same words — the exposure the owner accepted), is free of the pair.
    for line in seen.lines() {
        let is_argv = line.starts_with("argv:--user=")
            || line.starts_with("argv:--pw=")
            || line.starts_with("self-argv=");
        if !is_argv {
            assert!(!line.contains(USER) && !line.contains(PASS), "⚠ the pair leaked into: {line}");
        }
    }
    if std::path::Path::new("/proc/self/cmdline").exists() {
        let parent = seen.lines().find(|l| l.starts_with("parent-argv=")).expect("parent argv");
        assert!(parent.contains("ibc-start"), "{parent}");
        assert!(
            !parent.contains(USER) && !parent.contains(PASS),
            "⚠ the verb's own argv: {parent}"
        );
    }
    // The verb's streams, the state directory and the whole project tree (bar the database, which
    // holds the pair by design, and the probe the fake wrote on purpose).
    let streams = format!("{}{}", stdout(&ran), stderr(&ran));
    assert!(!streams.contains(USER) && !streams.contains(PASS), "⚠ a VALUE reached a stream");
    assert!(streams.contains("IBKR_DEMO_USERNAME") && streams.contains("never printed"));
    assert!(streams.contains("visible in `ps`"), "the exposure must be said: {streams}");
    assert!(!state_text(&c).contains(PASS));
    let tree = tree_text(c.dir(), &["db", "probe.out"]);
    assert!(!tree.contains(USER) && !tree.contains(PASS), "⚠ a VALUE was written into the project");
}

/// ⚠ THE HEADLINE. The pair is taken FROM the database, so a rotation through `secrets set` — the
/// only supported way to change a password — is the pair the very next launch uses.
#[test]
fn the_pair_a_rotation_wrote_is_the_pair_the_next_launch_uses() {
    let c = project("ibc-rotate");
    let probe = install_gateway(&c);
    ok(&ibc_start(&c, &[]));
    assert!(std::fs::read_to_string(&probe).unwrap().contains(&format!("--pw={PASS}\n")));

    let rotated = "sentinel-ROTATED-pass-5e5e";
    put(&c, &name("DEMO", "PASSWORD"), rotated);
    ok(&ibc_start(&c, &[]));
    let seen = std::fs::read_to_string(&probe).unwrap();
    assert!(seen.contains(&format!("argv:--pw={rotated}\n")), "{seen}");
    assert!(!seen.contains(PASS), "the OLD password survived a rotation: {seen}");
}

#[test]
fn a_password_with_spaces_and_shell_characters_travels_as_one_word() {
    let c = project("ibc-odd");
    let probe = install_gateway(&c);
    let odd = "p w$HOME;`id`\"'\\ #!";
    let key = name("DEMO", "PASSWORD");
    ok(&c.run(&["set", key.as_str(), "--from-env", "PW_FIXTURE"], None, &[("PW_FIXTURE", odd)]));
    ok(&ibc_start(&c, &[]));
    let seen = std::fs::read_to_string(&probe).unwrap();
    assert!(seen.contains(&format!("argv:--pw={odd}\n")), "argv is not shell-parsed: {seen}");
}

#[test]
fn no_value_reaches_any_stream_or_the_state_directory() {
    let c = project("ibc-silent");
    install_gateway(&c);
    let journal_before = c.journal_lines().len();

    // One run that succeeds, one whose child fails, and several refused — every outcome, every stream.
    let mut streams = String::new();
    for out in [
        ibc_start(&c, &[]),
        ibc_start(&c, &[("DRIVER_EXIT", "3")]),
        c.run(
            &["ibc-start", "--root", "/nope", "--gateway-version", "1045", "--java-path", "/j"],
            None,
            &[],
        ),
        c.run(
            &["ibc-start", "--root", "x", "--gateway-version", "10 45", "--java-path", "/j"],
            None,
            &[],
        ),
    ] {
        streams.push_str(&format!("{}{}", stdout(&out), stderr(&out)));
    }
    for sentinel in [USER, PASS] {
        assert!(!streams.contains(sentinel), "⚠ a VALUE reached stdout/stderr: {streams}");
        assert!(!state_text(&c).contains(sentinel), "⚠ a VALUE reached the state directory");
    }
    assert_eq!(
        c.journal_lines().len(),
        journal_before,
        "this verb changes no credential row, so it journals nothing"
    );
}

#[test]
fn the_refusals_name_the_repair_and_run_nothing() {
    // 1. No store at all: no IBKR session (no account row can exist), and it says how to make one.
    let bare = SetCase::new("ibc-no-store");
    let probe = install_gateway(&bare);
    let all = refused(&ibc_start(&bare, &[]));
    assert!(all.contains("secrets init") && all.contains("secrets set"), "{all}");
    assert!(all.contains("NO IBKR session"), "{all}");
    assert!(!probe.exists(), "the launcher ran although no login could be taken");

    // 2. A store without an ibkr row: no session, both names and the repair said; another
    //    credential in the same store is not even read into the process.
    let other = SetCase::new("ibc-no-names");
    ok(&other.run(&["init"], None, &[]));
    put(&other, "BINANCE_LIVE_API_KEY", "sentinel-venue-secret-1");
    let probe = install_gateway(&other);
    let all = refused(&ibc_start(&other, &[]));
    assert!(all.contains(&name("DEMO", "USERNAME")) && all.contains(&name("DEMO", "PASSWORD")));
    assert!(all.contains("vike-cli secrets set"), "{all}");
    assert!(!all.contains("sentinel-venue-secret-1"), "{all}");
    assert!(!probe.exists());

    // 2b. An ACTIVE ibkr DEMO row and no pair: the session is this verb's, the pair is not there —
    //     both names refused as not in the store, on the run-failure rung.
    let row_only = SetCase::new("ibc-row-only");
    ok(&row_only.run(&["init"], None, &[]));
    ok(&row_only.run(
        &["account", "add", "--venue", "ibkr", "--tier", "demo", "--no-label"],
        None,
        &[],
    ));
    let probe = install_gateway(&row_only);
    let all = failed(&ibc_start(&row_only, &[]));
    assert!(all.contains(&name("DEMO", "USERNAME")) && all.contains(&name("DEMO", "PASSWORD")));
    assert!(all.contains("not in the store") && all.contains("vike-cli secrets set"), "{all}");
    assert!(!probe.exists());

    // 3. A BLANK password (carried in from an old file — `set` itself refuses to write one).
    let blank = SetCase::new("ibc-blank");
    blank.store_rows(&format!(
        "{}={USER}\n{}=\n",
        name("DEMO", "USERNAME"),
        name("DEMO", "PASSWORD")
    ));
    let probe = install_gateway(&blank);
    let all = failed(&ibc_start(&blank, &[]));
    assert!(all.contains(&name("DEMO", "PASSWORD")), "{all}");
    assert!(!all.contains(USER), "{all}");
    assert!(!probe.exists());

    // 4. A store that EXISTS and cannot be read is an error, never 'no credentials' or 'no session'.
    let broken = SetCase::new("ibc-unreadable");
    std::fs::create_dir_all(broken.store().parent().unwrap()).unwrap();
    std::fs::write(broken.store(), b"this is not a sqlite database\n").unwrap();
    let probe = install_gateway(&broken);
    let all = failed(&ibc_start(&broken, &[]));
    assert!(all.contains("could not be read") && all.contains("not 'no credentials'"), "{all}");
    assert!(!probe.exists());
}

/// ⚠ THE PAIR RULE, `ibc-start` side: every session that is not `(socket, <its tier>)` is refused
/// BEFORE a value is read — naming the pair the store holds and the rows that set it — with nothing
/// run and no value on any stream.
#[test]
fn every_wrong_pair_is_refused_with_nothing_run() {
    // (socket, LIVE) — a LIVE pair alone: the plain (demo) start is not its gateway.
    let live = SetCase::new("ibc-live-only");
    live.store_rows(&format!(
        "{}={LIVE_USER}\n{}={LIVE_PASS}\n",
        name("LIVE", "USERNAME"),
        name("LIVE", "PASSWORD")
    ));
    let probe = install_live_gateway(&live);
    let all = refused(&ibc_start(&live, &[]));
    assert!(all.contains("(socket, demo)") && all.contains("holds (socket, live)"), "{all}");
    assert!(all.contains("ibc-start --tier live") && all.contains("account row"), "{all}");
    assert!(!all.contains(LIVE_USER) && !all.contains(LIVE_PASS), "{all}");
    assert!(!probe.exists());

    // (socket, demo) asked for LIVE.
    let demo = project("ibc-demo-asked-live");
    let probe = install_live_gateway(&demo);
    let all = refused(&ibc_start_tier(&demo, Some("live"), &[]));
    assert!(all.contains("(socket, live)") && all.contains("holds (socket, demo)"), "{all}");
    assert!(!all.contains(USER) && !all.contains(PASS), "{all}");
    assert!(!probe.exists());

    // (cpapi, demo): the Client Portal product rides this box, so no IB Gateway logs in.
    let cp = cp_project("ibc-on-cpapi");
    let probe = install_live_gateway(&cp);
    for tier in [None, Some("live")] {
        let all = refused(&ibc_start_tier(&cp, tier, &[]));
        assert!(all.contains("holds (cpapi, demo)"), "{all}");
        assert!(all.contains("ibkr-cp-login") && all.contains("venue.ibkr.demo.backend"), "{all}");
        assert!(!all.contains(USER) && !all.contains(PASS), "{all}");
    }
    assert!(!probe.exists(), "an IB Gateway was started on a cpapi box");

    // A backend row nobody can parse: no session at all, the row named.
    let bad = project("ibc-bad-backend");
    config_set(&bad, "venue.ibkr.demo.backend", "grpc");
    let probe = install_gateway(&bad);
    let all = refused(&ibc_start(&bad, &[]));
    assert!(all.contains("`grpc`") && all.contains("venue.ibkr.demo.backend"), "{all}");
    assert!(!probe.exists());
}

/// Active rows at demo AND live, under any labels, are a CONFLICT: no pick, so NEITHER tier of
/// EITHER verb logs in. Deactivating one tier's rows resolves it, and the demo start then hands the
/// child the demo pair and never the live one beside it in the store.
#[test]
fn a_demo_and_a_live_row_together_are_a_conflict_and_nothing_logs_in() {
    let c = SetCase::new("ibc-both");
    c.store_rows(&format!(
        "{}={USER}\n{}={PASS}\n{}={LIVE_USER}\n{}={LIVE_PASS}\n",
        name("DEMO", "USERNAME"),
        name("DEMO", "PASSWORD"),
        name("LIVE", "USERNAME"),
        name("LIVE", "PASSWORD")
    ));
    let probe = install_live_gateway(&c);
    let cp_probe = plant_login_driver(&c, PROBE_DRIVER);
    let (demo_id, live_id) = (ibkr_row(&c, "demo"), ibkr_row(&c, "live"));
    for tier in [None, Some("live")] {
        let all = refused(&ibc_start_tier(&c, tier, &[]));
        assert!(all.contains("CONFLICT"), "{all}");
        assert!(all.contains(&format!("demo: {demo_id}; live: {live_id}")), "{all}");
        let mut cp = vec!["ibkr-cp-login"];
        cp.extend(tier.map(|t| ["--tier", t]).into_iter().flatten());
        let all = refused(&c.run(&cp, None, &[]));
        assert!(all.contains("CONFLICT"), "{all}");
        for secret in [USER, PASS, LIVE_USER, LIVE_PASS] {
            assert!(!all.contains(secret), "⚠ a VALUE reached a stream");
        }
    }
    assert!(!probe.exists() && !cp_probe.exists(), "a login ran on a conflicting store");

    deactivate(&c, &live_id);
    ok(&ibc_start(&c, &[]));
    let seen = std::fs::read_to_string(&probe).unwrap();
    assert!(
        seen.contains(&format!("argv:--user={USER}\n")) && seen.contains("argv:--mode=paper\n")
    );
    assert!(
        !seen.contains(LIVE_USER) && !seen.contains(LIVE_PASS),
        "the LIVE pair was used: {seen}"
    );
}

/// `--check` is the pair check alone: it reports the resolved pair and its rows on stdout, exits 0
/// when the pair is the verb's and on the `refused` rung otherwise, reads no secret (it answers on a
/// store holding an account row and no pair at all) and runs nothing.
#[test]
fn check_reports_the_pair_and_reads_no_value() {
    let c = project("ibc-check");
    let probe = install_live_gateway(&c);
    let cp_probe = plant_login_driver(&c, PROBE_DRIVER);
    let id = ibkr_row(&c, "demo");

    let out = c.run(&["ibc-start", "--check"], None, &[]);
    ok(&out);
    let text = stdout(&out);
    assert!(text.contains("ibkr session in the settings database: (socket, demo)"), "{text}");
    assert!(text.contains(&format!("account row {id}: ibkr demo, unlabelled, active")), "{text}");
    assert!(text.contains("venue.ibkr.demo.backend: not set (socket, the default)"), "{text}");
    assert!(text.contains("is this verb's session"), "{text}");
    for (args, want) in [
        (&["ibc-start", "--check", "--tier", "live"][..], "(socket, live)"),
        (&["ibkr-cp-login", "--check"][..], "(cpapi, demo)"),
        (&["ibkr-cp-login", "--check", "--tier", "live"][..], "(cpapi, live)"),
    ] {
        let out = c.run(args, None, &[]);
        let all = refused(&out);
        assert!(all.contains(want) && all.contains("holds (socket, demo)"), "{args:?}: {all}");
        assert!(
            stdout(&out).contains("ibkr session in the settings database: (socket, demo)"),
            "--check writes the pair even when it refuses: {}",
            stdout(&out)
        );
    }

    config_set(&c, "venue.ibkr.demo.backend", "cpapi");
    ok(&c.run(&["ibkr-cp-login", "--check"], None, &[]));
    let all = refused(&c.run(&["ibc-start", "--check"], None, &[]));
    assert!(all.contains("holds (cpapi, demo)") && all.contains("= cpapi"), "{all}");
    let streams = [
        c.run(&["ibc-start", "--check"], None, &[]),
        c.run(&["ibkr-cp-login", "--check"], None, &[]),
    ]
    .iter()
    .map(|o| format!("{}{}", stdout(o), stderr(o)))
    .collect::<String>();
    assert!(!streams.contains(USER) && !streams.contains(PASS), "⚠ a VALUE reached a stream");
    assert!(!probe.exists() && !cp_probe.exists(), "--check ran a launcher");

    // It needs no pair at all: an account row alone answers.
    let row_only = SetCase::new("ibc-check-no-pair");
    ok(&row_only.run(&["init"], None, &[]));
    ok(&row_only.run(
        &["account", "add", "--venue", "ibkr", "--tier", "live", "--no-label"],
        None,
        &[],
    ));
    ok(&row_only.run(&["ibc-start", "--check", "--tier", "live"], None, &[]));

    // No session, and a store that cannot be read: refused and failed, as the start is.
    let bare = SetCase::new("ibc-check-bare");
    let all = refused(&bare.run(&["ibc-start", "--check"], None, &[]));
    assert!(all.contains("ibkr session in the settings database: off"), "{all}");
    let broken = SetCase::new("ibc-check-unreadable");
    std::fs::create_dir_all(broken.store().parent().unwrap()).unwrap();
    std::fs::write(broken.store(), b"this is not a sqlite database\n").unwrap();
    let all = failed(&broken.run(&["ibkr-cp-login", "--check"], None, &[]));
    assert!(all.contains("could not be read"), "{all}");
}

/// ⚠ THE LIVE HEADLINE. `--tier live` hands the child the LIVE pair, the LIVE ini, its own settings
/// directory and `--mode=live` — on its ARGV (the owner's accepted exposure, 2026-10-08) and nowhere
/// else — and never the demo pair that sits beside it in the same store (its row deactivated: one box
/// rides one tier).
#[test]
fn the_live_child_receives_the_live_pair_on_its_argv_and_nowhere_else() {
    let c = project("ibc-live-argv"); // the DEMO pair, as an operator has it …
    put(&c, &name("LIVE", "USERNAME"), LIVE_USER); // … and the LIVE pair beside it,
    put(&c, &name("LIVE", "PASSWORD"), LIVE_PASS);
    deactivate(&c, &ibkr_row(&c, "demo")); // … the box riding the LIVE tier.
    let probe = install_live_gateway(&c);
    let ran = ibc_start_tier(&c, Some("live"), &[]);
    ok(&ran);

    let seen = std::fs::read_to_string(&probe).expect("the launcher ran and wrote its probe");
    let root = gw_root(&c);
    for want in [
        format!("argv:--user={LIVE_USER}\n"),
        format!("argv:--pw={LIVE_PASS}\n"),
        "argv:--mode=live\n".to_string(),
        "argv:--gateway\n".to_string(),
        "argv:1045\n".to_string(),
        "argv:--java-path=/jre/bin\n".to_string(),
        format!("argv:--ibc-ini={}\n", root.join("ibc").join("config-live.ini").display()),
        format!("argv:--ibc-path={}\n", root.join("ibc").display()),
        // The program directory is shared; the SETTINGS directory is the live gateway's own.
        format!("argv:--tws-path={}\n", root.join("jts").display()),
        format!("argv:--tws-settings-path={}\n", root.join("jts-live").display()),
    ] {
        assert!(seen.contains(&want), "the live launcher was not handed {want:?}: {seen}");
    }
    assert!(!seen.contains("--mode=paper"), "a live start must not ask for paper mode: {seen}");
    // …and ONLY there: every line that is not an `argv:` login word, or the launcher's own command
    // line (those same words — the accepted exposure), is free of the LIVE pair, and NO line holds
    // the demo pair at all.
    for line in seen.lines() {
        assert!(
            !line.contains(USER) && !line.contains(PASS),
            "⚠ the DEMO pair reached the live launcher: {line}"
        );
        let is_argv = line.starts_with("argv:--user=")
            || line.starts_with("argv:--pw=")
            || line.starts_with("self-argv=");
        if !is_argv {
            assert!(
                !line.contains(LIVE_USER) && !line.contains(LIVE_PASS),
                "⚠ the live pair leaked into: {line}"
            );
        }
    }
    if std::path::Path::new("/proc/self/cmdline").exists() {
        let parent = seen.lines().find(|l| l.starts_with("parent-argv=")).expect("parent argv");
        assert!(parent.contains("ibc-start") && parent.contains("live"), "{parent}");
        assert!(
            !parent.contains(LIVE_USER) && !parent.contains(LIVE_PASS),
            "⚠ the verb's own argv: {parent}"
        );
    }
    let streams = format!("{}{}", stdout(&ran), stderr(&ran));
    for secret in [USER, PASS, LIVE_USER, LIVE_PASS] {
        assert!(!streams.contains(secret), "⚠ a VALUE reached a stream");
        assert!(!state_text(&c).contains(secret), "⚠ a VALUE reached the state directory");
    }
    assert!(
        streams.contains(&name("LIVE", "USERNAME")) && streams.contains("never printed"),
        "{streams}"
    );
    assert!(
        streams.contains("REAL-MONEY") && streams.contains("phone"),
        "the live start must say it is real money and that the phone approves it: {streams}"
    );
    assert!(streams.contains("visible in `ps`"), "the exposure must be said: {streams}");
    let tree = tree_text(c.dir(), &["db", "probe.out"]);
    for secret in [LIVE_USER, LIVE_PASS] {
        assert!(!tree.contains(secret), "⚠ a VALUE was written into the project");
    }
}

/// A rotation of the LIVE password is the pair the next LIVE launch uses; the demo password is not
/// involved. (The same headline as the demo's, for the new pair.)
#[test]
fn a_rotated_live_password_is_the_one_the_next_live_launch_uses() {
    let c = live_project("ibc-live-rotate");
    let probe = install_live_gateway(&c);
    ok(&ibc_start_tier(&c, Some("live"), &[]));
    assert!(std::fs::read_to_string(&probe).unwrap().contains(&format!("--pw={LIVE_PASS}\n")));
    let rotated = "sentinel-LIVE-ROTATED-9a9a";
    put(&c, &name("LIVE", "PASSWORD"), rotated);
    ok(&ibc_start_tier(&c, Some("live"), &[]));
    let seen = std::fs::read_to_string(&probe).unwrap();
    assert!(seen.contains(&format!("argv:--pw={rotated}\n")), "{seen}");
    assert!(!seen.contains(LIVE_PASS), "the OLD live password survived a rotation: {seen}");
}

/// `--tier demo` is the default spelled out: the child is handed exactly the words the plain verb
/// hands it. `--tier live` on that demo box is refused; once the box rides the live tier, it is a
/// different set of words.
#[test]
fn an_explicit_demo_tier_is_the_plain_verb_and_live_is_not() {
    let c = project("ibc-tier-demo");
    let probe = install_live_gateway(&c);
    let argv_of = |tier: Option<&str>| {
        ok(&ibc_start_tier(&c, tier, &[]));
        std::fs::read_to_string(&probe)
            .unwrap()
            .lines()
            .filter(|l| l.starts_with("argv:"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let plain = argv_of(None);
    assert_eq!(argv_of(Some("demo")), plain, "--tier demo must be the plain verb");
    assert_eq!(argv_of(Some("DEMO")), plain, "the word is case-insensitive");
    assert!(plain.contains(&"argv:--mode=paper".to_string()), "{plain:?}");
    refused(&ibc_start_tier(&c, Some("live"), &[]));

    put(&c, &name("LIVE", "USERNAME"), LIVE_USER);
    put(&c, &name("LIVE", "PASSWORD"), LIVE_PASS);
    deactivate(&c, &ibkr_row(&c, "demo"));
    let live = argv_of(Some("live"));
    assert_ne!(live, plain);
    assert!(live.contains(&"argv:--mode=live".to_string()), "{live:?}");
}

/// A tier word that is no gateway is a USAGE error and runs nothing — `paper` is an `account` tier.
#[test]
fn a_tier_that_is_no_gateway_is_a_usage_error_and_runs_nothing() {
    let c = project("ibc-tier-bad");
    let probe = install_live_gateway(&c);
    let out = ibc_start_tier(&c, Some("paper"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("demo") && stderr(&out).contains("live"), "{}", stderr(&out));
    assert!(!probe.exists(), "the launcher ran for a tier that is no gateway");
}

/// The refusals of a LIVE start each name the cause, print no value, and run nothing.
#[test]
fn the_live_refusals_name_the_cause_and_run_nothing() {
    let live_names = [name("LIVE", "USERNAME"), name("LIVE", "PASSWORD")];

    // 1. No store at all: no session; it says how to make one, and names the LIVE keys.
    let bare = SetCase::new("ibc-live-no-store");
    let probe = install_live_gateway(&bare);
    let all = refused(&ibc_start_tier(&bare, Some("live"), &[]));
    assert!(all.contains("secrets init") && all.contains(&live_names[0]), "{all}");
    assert!(!probe.exists(), "the launcher ran although no login could be taken");

    // 2. The DEMO pair alone: the box rides (socket, demo), and the LIVE gateway never falls back to
    //    the demo pair — refused before either pair is read.
    let demo_only = project("ibc-live-demo-only");
    let probe = install_live_gateway(&demo_only);
    let all = refused(&ibc_start_tier(&demo_only, Some("live"), &[]));
    assert!(all.contains("(socket, live)") && all.contains("holds (socket, demo)"), "{all}");
    assert!(!all.contains(USER) && !all.contains(PASS), "⚠ the demo pair was read: {all}");
    assert!(!probe.exists());

    // 2b. An ACTIVE LIVE row with no live pair: both names refused as not in the store.
    let row_only = SetCase::new("ibc-live-row-only");
    ok(&row_only.run(&["init"], None, &[]));
    ok(&row_only.run(
        &["account", "add", "--venue", "ibkr", "--tier", "live", "--no-label"],
        None,
        &[],
    ));
    let probe = install_live_gateway(&row_only);
    let all = failed(&ibc_start_tier(&row_only, Some("live"), &[]));
    assert!(live_names.iter().all(|n| all.contains(n.as_str())), "{all}");
    assert!(all.contains("not in the store") && all.contains("LIVE (REAL-MONEY)"), "{all}");
    assert!(!probe.exists());

    // 3. A BLANK live password (carried in from an old file — `set` itself refuses to write one).
    let blank = SetCase::new("ibc-live-blank");
    blank.store_rows(&format!("{}={LIVE_USER}\n{}=\n", live_names[0], live_names[1]));
    let probe = install_live_gateway(&blank);
    let all = failed(&ibc_start_tier(&blank, Some("live"), &[]));
    assert!(all.contains(&live_names[1]) && all.contains("BLANK"), "{all}");
    assert!(!all.contains(LIVE_USER), "{all}");
    assert!(!probe.exists());

    // 4. The LIVE account row is DEACTIVATED: no session, the row named with the repair; re-activated,
    //    it starts.
    let off = live_project("ibc-live-inactive");
    let probe = install_live_gateway(&off);
    let id = ibkr_row(&off, "live");
    deactivate(&off, &id);
    let all = refused(&ibc_start_tier(&off, Some("live"), &[]));
    assert!(all.contains("(socket, live)") && all.contains("DEACTIVATED"), "{all}");
    assert!(all.contains("account activate"), "{all}");
    assert!(!all.contains(LIVE_USER) && !all.contains(LIVE_PASS), "{all}");
    assert!(!probe.exists(), "the launcher ran for an account the daemon will not arm");
    ok(&off.run(&["account", "activate", "--id", id.as_str()], None, &[]));
    ok(&ibc_start_tier(&off, Some("live"), &[]));
    assert!(probe.exists());

    // 5. The live gateway's own files are required BEFORE a value is read: no live ini, then no live
    //    settings directory — each named by path, with the script that creates it.
    let bare_install = live_project("ibc-live-install");
    let probe = install_gateway(&bare_install); // the DEMO install only
    let all = failed(&ibc_start_tier(&bare_install, Some("live"), &[]));
    assert!(all.contains("config-live.ini") && all.contains("write-ibc-config.sh live"), "{all}");
    std::fs::write(
        gw_root(&bare_install).join("ibc").join("config-live.ini"),
        "IbLoginId=\nIbPassword=\n",
    )
    .unwrap();
    let all = failed(&ibc_start_tier(&bare_install, Some("live"), &[]));
    assert!(all.contains("jts-live") && all.contains("start-gateway.sh live"), "{all}");
    assert!(!all.contains(LIVE_USER) && !all.contains(LIVE_PASS), "{all}");
    assert!(!probe.exists());
}

#[test]
fn a_deactivated_account_is_not_logged_in_to() {
    let c = project("ibc-inactive");
    let probe = install_gateway(&c);
    // The ibkr DEMO account row the two credentials created, by the shipped binary's own listing.
    let id = ibkr_row(&c, "demo");
    deactivate(&c, &id);

    let all = refused(&ibc_start(&c, &[]));
    assert!(all.contains("DEACTIVATED") && all.contains("account activate"), "{all}");
    assert!(!probe.exists(), "the launcher ran for an account the daemon will not arm");

    ok(&c.run(&["account", "activate", "--id", id.as_str()], None, &[]));
    ok(&ibc_start(&c, &[]));
    assert!(probe.exists());
}

#[test]
fn a_bad_install_or_operand_is_refused_before_a_value_is_read() {
    let c = project("ibc-install");
    // No install at all: named by path, and no store read (the store is fine here, so the proof is
    // that the message is about the install).
    let all = failed(&ibc_start(&c, &[]));
    assert!(all.contains("missing") && all.contains("ibcstart.sh"), "{all}");
    // A version that is not a version is a usage error.
    let out = c.run(
        &["ibc-start", "--root", "x", "--gateway-version", "10;45", "--java-path", "/j"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    // The child's verdict is the verb's.
    install_gateway(&c);
    let all = failed(&ibc_start(&c, &[("DRIVER_EXIT", "3")]));
    assert!(all.contains("exited with") && !all.contains(PASS) && !all.contains(USER), "{all}");
}

// --- ibkr-cp-login ---

/// Plant the one script `ibkr-cp-login` may run, where it looks for it — `<project>/bin/ibkr-cpapi/`.
/// No exec bit: the verb runs it through `bash`, because an scp lands these scripts 0644. It writes
/// what it was given to a file BESIDE ITSELF (no environment variable names the probe).
fn plant_login_driver(c: &SetCase, body: &str) -> PathBuf {
    let tool = c.dir().join("bin").join("ibkr-cpapi");
    std::fs::create_dir_all(&tool).unwrap();
    std::fs::write(tool.join("run-login.sh"), body).unwrap();
    tool.join("probe.out")
}

const PROBE_DRIVER: &str = "#!/usr/bin/env bash\n\
    here=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\n\
    {\n\
      printf 'user=%s\\n' \"$IBKR_CP_USER\"\n\
      printf 'pass=%s\\n' \"$IBKR_CP_PASS\"\n\
      printf 'paper=%s\\n' \"${PAPER-unset}\"\n\
      printf 'argc=%s\\n' \"$#\"\n\
      if [ -r /proc/$$/cmdline ]; then printf 'self-argv=%s\\n' \"$(tr '\\0' ' ' < /proc/$$/cmdline)\"; fi\n\
      if [ -r /proc/$PPID/cmdline ]; then printf 'parent-argv=%s\\n' \"$(tr '\\0' ' ' < /proc/$PPID/cmdline)\"; fi\n\
    } > \"$here/probe.out\"\n\
    exit ${DRIVER_EXIT:-0}\n";

#[test]
fn the_cp_child_receives_the_pair_by_environment_and_never_on_argv() {
    let c = cp_project("cp-env");
    let probe = plant_login_driver(&c, PROBE_DRIVER);
    // An ambient PAPER must not matter: the account is the DEMO one, so the toggle is always set.
    let ran = c.run(&["ibkr-cp-login"], None, &[("PAPER", "0")]);
    ok(&ran);

    let seen = std::fs::read_to_string(&probe).expect("the driver ran and wrote its probe");
    assert!(seen.contains(&format!("user={USER}\n")), "the pair did not reach the child: {seen}");
    assert!(seen.contains(&format!("pass={PASS}\n")), "{seen}");
    assert!(seen.contains("paper=1\n"), "the DEMO account ticks the Paper toggle: {seen}");
    assert!(seen.contains("argc=0\n"), "the driver is run with no arguments: {seen}");
    if Path::new("/proc/self/cmdline").exists() {
        let argv: Vec<&str> = seen
            .lines()
            .filter(|l| l.starts_with("self-argv=") || l.starts_with("parent-argv="))
            .collect();
        assert_eq!(argv.len(), 2, "the probe could not read its argv: {seen}");
        for line in argv {
            assert!(!line.contains(USER) && !line.contains(PASS), "⚠ a VALUE is on argv: {line}");
        }
    }
    let text = format!("{}{}", stdout(&ran), stderr(&ran));
    assert!(!text.contains(USER) && !text.contains(PASS), "⚠ a VALUE reached a stream: {text}");
    assert!(!state_text(&c).contains(PASS), "⚠ a VALUE reached the state directory");
}

/// ⚠ THE CP LIVE HEADLINE. `ibkr-cp-login --tier live` on a `(cpapi, live)` box hands the driver the
/// LIVE pair and NO `PAPER` — an ambient one is removed, since it would tick the Paper toggle on the
/// real-money form — never the demo pair beside it in the store, and says the phone approves it.
#[test]
fn the_live_cp_child_gets_the_live_pair_and_no_paper() {
    let c = project("cp-live-env"); // the DEMO pair, its row deactivated below …
    put(&c, &name("LIVE", "USERNAME"), LIVE_USER);
    put(&c, &name("LIVE", "PASSWORD"), LIVE_PASS);
    deactivate(&c, &ibkr_row(&c, "demo"));
    config_set(&c, "venue.ibkr.live.backend", "cpapi");
    let probe = plant_login_driver(&c, PROBE_DRIVER);

    let ran = c.run(&["ibkr-cp-login", "--tier", "live"], None, &[("PAPER", "1")]);
    ok(&ran);
    let seen = std::fs::read_to_string(&probe).expect("the driver ran and wrote its probe");
    assert!(seen.contains(&format!("user={LIVE_USER}\n")), "{seen}");
    assert!(seen.contains(&format!("pass={LIVE_PASS}\n")), "{seen}");
    assert!(seen.contains("paper=unset\n"), "the LIVE child must get no PAPER at all: {seen}");
    assert!(!seen.contains(USER) && !seen.contains(PASS), "⚠ the DEMO pair reached it: {seen}");
    let text = format!("{}{}", stdout(&ran), stderr(&ran));
    for secret in [USER, PASS, LIVE_USER, LIVE_PASS] {
        assert!(!text.contains(secret), "⚠ a VALUE reached a stream");
    }
    assert!(text.contains(&name("LIVE", "USERNAME")) && text.contains("never printed"), "{text}");
    assert!(text.contains("REAL-MONEY") && text.contains("phone"), "{text}");
    assert!(text.contains("NEVER retried"), "{text}");
}

/// ⚠ THE PAIR RULE, `ibkr-cp-login` side: every session that is not `(cpapi, <its tier>)` is refused
/// before a value is read, naming the pair the store holds, with the driver never run.
#[test]
fn the_cp_login_refuses_every_pair_that_is_not_cpapi() {
    // (socket, demo): the IB Gateway rides this box — a cpapi login would evict it.
    let socket = project("cp-on-socket");
    let probe = plant_login_driver(&socket, PROBE_DRIVER);
    for tier in [&[][..], &["--tier", "live"][..]] {
        let mut args = vec!["ibkr-cp-login"];
        args.extend(tier);
        let all = refused(&socket.run(&args, None, &[]));
        assert!(all.contains("holds (socket, demo)") && all.contains("ibc-start"), "{all}");
        assert!(!all.contains(USER) && !all.contains(PASS), "{all}");
    }
    assert!(!probe.exists(), "the driver ran on a socket box");

    // (cpapi, demo) asked for LIVE.
    let cp = cp_project("cp-demo-asked-live");
    let probe = plant_login_driver(&cp, PROBE_DRIVER);
    let all = refused(&cp.run(&["ibkr-cp-login", "--tier", "live"], None, &[]));
    assert!(all.contains("(cpapi, live)") && all.contains("holds (cpapi, demo)"), "{all}");
    assert!(!probe.exists());

    // A backend row nobody can parse.
    let bad = project("cp-bad-backend");
    config_set(&bad, "venue.ibkr.demo.backend", "grpc");
    let probe = plant_login_driver(&bad, PROBE_DRIVER);
    let all = refused(&bad.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("`grpc`"), "{all}");
    assert!(!probe.exists());
}

#[test]
fn the_cp_child_is_never_run_when_the_pair_cannot_be_taken() {
    // No ibkr row → no session, refused BEFORE the driver runs.
    let empty = SetCase::new("cp-empty");
    ok(&empty.run(&["init"], None, &[]));
    let probe = plant_login_driver(&empty, PROBE_DRIVER);
    let all = refused(&empty.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("vike-cli secrets set"), "{all}");
    assert!(!probe.exists(), "the driver ran although the pair could not be taken");

    // A cpapi DEMO row with no pair → the session is this verb's, the pair is not there.
    let row_only = SetCase::new("cp-row-only");
    ok(&row_only.run(&["init"], None, &[]));
    ok(&row_only.run(
        &["account", "add", "--venue", "ibkr", "--tier", "demo", "--no-label"],
        None,
        &[],
    ));
    config_set(&row_only, "venue.ibkr.demo.backend", "cpapi");
    let probe = plant_login_driver(&row_only, PROBE_DRIVER);
    let all = failed(&row_only.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("not in the store") && all.contains("vike-cli secrets set"), "{all}");
    assert!(!probe.exists());

    // A LIVE pair alone is not the demo login: the box rides (socket, live).
    let live = SetCase::new("cp-live-only");
    live.store_rows(&format!(
        "{}={LIVE_USER}\n{}={LIVE_PASS}\n",
        name("LIVE", "USERNAME"),
        name("LIVE", "PASSWORD")
    ));
    let probe = plant_login_driver(&live, PROBE_DRIVER);
    let all = refused(&live.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("holds (socket, live)") && !all.contains(LIVE_PASS), "{all}");
    assert!(!probe.exists());

    // No driver installed → says where it looked.
    let none = cp_project("cp-no-driver");
    let all = failed(&none.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("no login driver") && all.contains("ibkr-cpapi"), "{all}");
}

#[test]
fn the_cp_drivers_verdict_is_the_verbs() {
    let c = cp_project("cp-exit");
    plant_login_driver(&c, PROBE_DRIVER);
    let all = failed(&c.run(&["ibkr-cp-login"], None, &[("DRIVER_EXIT", "3")]));
    assert!(all.contains("exited with"), "{all}");
    assert!(!all.contains(PASS) && !all.contains(USER), "{all}");
}
