//! `secrets ibc-start` and `secrets ibkr-cp-login`, asserted over the SHIPPED binary: the gateway's
//! login is taken from the settings database (so a rotation through `secrets set` is the pair the
//! next launch uses), the child receives it on its ARGV (IBC) or in its ENVIRONMENT (Client Portal)
//! and nowhere else, no value reaches any stream, the state directory or the project tree, and every
//! refusal names the repair and runs nothing.
//!
//! ⚠ No credential NAME is spelled as one literal: they are composed by [`name`], because an
//! env-shaped literal in this tree is read by the settings registry's sweep as a read, and the
//! planted VALUES are obvious sentinels — nothing here is, or resembles, a real login.
//!
//! The "child" is a fake `ibcstart.sh` / `run-login.sh` planted where the verb looks for it. It runs
//! through `bash` (no exec bit needed) and writes what it was given to a file beside itself.

use std::path::{Path, PathBuf};

use super::support::{SetCase, exit_code, rung};
use super::{stderr, stdout};

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

/// A project with a settings database that holds the DEMO pair, put in the way an operator puts one
/// in: the empty store created by `migrate --init`, each value on stdin to `secrets set`.
fn project(tag: &str) -> SetCase {
    let c = SetCase::new(tag);
    ok(&c.run(&["migrate", "--init"], None, &[]));
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
    let root = gw_root(c);
    let root = root.to_str().expect("utf-8");
    c.run(
        &["ibc-start", "--root", root, "--gateway-version", "1045", "--java-path", "/jre/bin"],
        None,
        envs,
    )
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
    // 1. No store at all: nothing to take a login from, and it says how to make one.
    let bare = SetCase::new("ibc-no-store");
    let probe = install_gateway(&bare);
    let all = failed(&ibc_start(&bare, &[]));
    assert!(all.contains("migrate --init") && all.contains("secrets set"), "{all}");
    assert!(!probe.exists(), "the launcher ran although no login could be taken");

    // 2. A store without the NAMES: both are named, and so is the repair; another credential in the
    //    same store is not even read into the process.
    let other = SetCase::new("ibc-no-names");
    ok(&other.run(&["migrate", "--init"], None, &[]));
    put(&other, "BINANCE_LIVE_API_KEY", "sentinel-venue-secret-1");
    let probe = install_gateway(&other);
    let all = failed(&ibc_start(&other, &[]));
    assert!(all.contains(&name("DEMO", "USERNAME")) && all.contains(&name("DEMO", "PASSWORD")));
    assert!(all.contains("not in the store") && all.contains("vike-cli secrets set"), "{all}");
    assert!(!all.contains("sentinel-venue-secret-1"), "{all}");
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

    // 4. A store that EXISTS and cannot be read is an error, never 'no credentials'.
    let broken = SetCase::new("ibc-unreadable");
    std::fs::create_dir_all(broken.store().parent().unwrap()).unwrap();
    std::fs::write(broken.store(), b"this is not a sqlite database\n").unwrap();
    let probe = install_gateway(&broken);
    let all = failed(&ibc_start(&broken, &[]));
    assert!(all.contains("could not be read") && all.contains("not 'no credentials'"), "{all}");
    assert!(!probe.exists());

    // 5. A LIVE pair alone is not used: the ibkr mount arms the DEMO tier only (and names a LIVE-only
    //    store as not wired), so the gateway has no account to log in to — and says why.
    let live = SetCase::new("ibc-live-only");
    live.store_rows(&format!(
        "{}={LIVE_USER}
{}={LIVE_PASS}
",
        name("LIVE", "USERNAME"),
        name("LIVE", "PASSWORD")
    ));
    let probe = install_gateway(&live);
    let all = failed(&ibc_start(&live, &[]));
    assert!(all.contains(&name("DEMO", "USERNAME")) && all.contains("DEMO tier"), "{all}");
    assert!(!all.contains(LIVE_USER) && !all.contains(LIVE_PASS), "{all}");
    assert!(!probe.exists());
}

#[test]
fn a_demo_pair_beside_a_live_one_logs_in_to_the_demo_account() {
    let c = SetCase::new("ibc-both");
    c.store_rows(&format!(
        "{}={USER}
{}={PASS}
{}={LIVE_USER}
{}={LIVE_PASS}
",
        name("DEMO", "USERNAME"),
        name("DEMO", "PASSWORD"),
        name("LIVE", "USERNAME"),
        name("LIVE", "PASSWORD")
    ));
    let probe = install_gateway(&c);
    let ran = ibc_start(&c, &[]);
    ok(&ran);
    let seen = std::fs::read_to_string(&probe).unwrap();
    assert!(
        seen.contains(&format!("argv:--user={USER}\n")) && seen.contains("argv:--mode=paper\n")
    );
    assert!(
        !seen.contains(LIVE_USER) && !seen.contains(LIVE_PASS),
        "the LIVE pair was used: {seen}"
    );
}

#[test]
fn a_deactivated_account_is_not_logged_in_to() {
    let c = project("ibc-inactive");
    let probe = install_gateway(&c);
    // The ibkr DEMO account row the two credentials created — its id comes out of the shipped
    // binary's own `accounts` listing: an indented line, first token an integer, naming ibkr.
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let id: i64 = listed
        .lines()
        .filter(|l| l.starts_with(' ') && l.contains("ibkr"))
        .filter_map(|l| l.split_whitespace().next().and_then(|t| t.parse().ok()))
        .next()
        .unwrap_or_else(|| panic!("no ibkr account row in the listing: {listed}"));
    let id = id.to_string();
    ok(&c.run(&["account", "deactivate", "--id", id.as_str()], None, &[]));

    let all = failed(&ibc_start(&c, &[]));
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
    let c = project("cp-env");
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

#[test]
fn the_cp_child_is_never_run_when_the_pair_cannot_be_taken() {
    // No pair in the store → refused BEFORE the driver runs.
    let empty = SetCase::new("cp-empty");
    ok(&empty.run(&["migrate", "--init"], None, &[]));
    let probe = plant_login_driver(&empty, PROBE_DRIVER);
    let all = failed(&empty.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("vike-cli secrets set"), "{all}");
    assert!(!probe.exists(), "the driver ran although the pair could not be taken");

    // A LIVE pair alone is not used (the mount arms DEMO only).
    let live = SetCase::new("cp-live-only");
    live.store_rows(&format!(
        "{}={LIVE_USER}
{}={LIVE_PASS}
",
        name("LIVE", "USERNAME"),
        name("LIVE", "PASSWORD")
    ));
    let probe = plant_login_driver(&live, PROBE_DRIVER);
    let all = failed(&live.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("DEMO tier") && !all.contains(LIVE_PASS), "{all}");
    assert!(!probe.exists());

    // No driver installed → says where it looked.
    let none = project("cp-no-driver");
    let all = failed(&none.run(&["ibkr-cp-login"], None, &[]));
    assert!(all.contains("no login driver") && all.contains("ibkr-cpapi"), "{all}");
}

#[test]
fn the_cp_drivers_verdict_is_the_verbs() {
    let c = project("cp-exit");
    plant_login_driver(&c, PROBE_DRIVER);
    let all = failed(&c.run(&["ibkr-cp-login"], None, &[("DRIVER_EXIT", "3")]));
    assert!(all.contains("exited with"), "{all}");
    assert!(!all.contains(PASS) && !all.contains(USER), "{all}");
}
