//! WHICH store a verb naming no `--store` acts on: the one the datahub serves. `--store`, else the
//! `config.store_root` row, else the project's own folder. Until 2026-11-17 `rename`, `rm` and
//! `repair` skipped the row, so on the CI box `vike-cli data history rename --dry-run` read
//! `<project>/market_data/hist` while the daemon served the row's `data/hist`, and answered
//! "nothing to rename".

use std::process::Stdio;
use std::time::Duration;

use super::*;

/// `store rename --venue demo --symbol BTCUSDT --to BTCUSD --dry-run`, then `rest`.
fn rename_dry_run<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["store", "rename", "--venue", "demo", "--symbol", "BTCUSDT"];
    args.extend_from_slice(&["--to", "BTCUSD", "--dry-run"]);
    args.extend_from_slice(rest);
    args
}

/// A scratch project whose row names A (the test tape) while its own folder B is an EMPTY store:
/// `(scratch, A, B)`.
fn row_names_a_beside_an_empty_b() -> (Scratch, PathBuf, PathBuf) {
    let scratch = Scratch::project();
    let a = scratch.dir.path().join("data").join("hist");
    seed_test_tape(&a);
    let b = scratch.dir.path().join("market_data").join("hist");
    std::fs::create_dir_all(&b).expect("the project's own empty store");
    scratch.row_names_store(&a);
    (scratch, a, b)
}

/// The row's store is the one `rename` plans over and names, the empty project folder is named
/// beside it as the second store, and `--store` outranks the row.
#[test]
fn rename_naming_no_store_follows_the_row_and_store_overrides_it() {
    let (scratch, a, b) = row_names_a_beside_an_empty_b();

    let out = scratch.tool().args(rename_dry_run(&[])).output().expect("run rename");
    let (stdout, stderr) = (stdout_of(&out), stderr_of(&out));
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stdout.starts_with(&format!("store: {} (stated for this box", a.display())),
        "{stdout}"
    );
    assert!(stdout.contains("demo BTCUSDT -> BTCUSD"), "the row's series is planned: {stdout}");
    assert!(stdout.contains("nothing was renamed"), "{stdout}");
    assert!(stderr.contains("two stores exist"), "{stderr}");
    for root in [&a, &b] {
        assert!(stderr.contains(&root.display().to_string()), "names {}: {stderr}", root.display());
    }

    let b_arg = b.to_str().expect("a UTF-8 path");
    let out = scratch.tool().args(rename_dry_run(&["--store", b_arg])).output().expect("run");
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(stdout.starts_with(&format!("store: {b_arg} (stated for this run")), "{stdout}");
    assert!(stdout.contains("nothing to rename"), "--store names B, which is empty: {stdout}");
    assert!(!stderr_of(&out).contains("two stores exist"), "{}", stderr_of(&out));
}

/// With no row the project's own folder answers, exactly as it does for the daemon.
#[test]
fn rename_with_no_row_reads_the_project_folder() {
    let scratch = Scratch::project();
    let b = scratch.dir.path().join("market_data").join("hist");
    seed_test_tape(&b);

    let out = scratch.tool().args(rename_dry_run(&[])).output().expect("run rename");
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(stdout.starts_with(&format!("store: {} (", b.display())), "{stdout}");
    assert!(stdout.contains("demo BTCUSDT -> BTCUSD"), "{stdout}");
}

/// `rm --dry-run` and `repair` (a rehearsal by default) lead with the row's store too.
#[test]
fn rm_and_repair_naming_no_store_follow_the_row() {
    let (scratch, a, _b) = row_names_a_beside_an_empty_b();
    let lead = format!("store: {} (stated for this box", a.display());
    for verb in ["rm", "repair"] {
        let mut args = vec!["store", verb, "--kind", "bar", "--venue", "demo", "--symbol"];
        args.extend_from_slice(&["BTCUSDT", "--interval", "1h", "--dry-run"]);
        let out = scratch.tool().args(&args).output().expect("run the verb");
        assert_eq!(out.status.code(), Some(0), "{verb}: {}", stderr_of(&out));
        assert!(stdout_of(&out).starts_with(&lead), "{verb}: {}", stdout_of(&out));
    }
}

/// `vike-cli data history ls` is `inventory()` over the wire, so it reads whatever the datahub serves:
/// a datahub started with no `--store` serves the row's tree, never the empty project folder. Read
/// off the daemon's own `listening … store=` line: a fresh project's datahub holds an access list,
/// so a key-less dial is refused at the handshake.
#[test]
fn a_datahub_naming_no_store_serves_the_rows_tree() {
    let scratch = Scratch::project();
    let a = scratch.dir.path().join("data").join("hist");
    std::fs::create_dir_all(&a).expect("the row's store");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a loopback port")
        .port();
    let addr = format!("127.0.0.1:{port}");
    scratch.serves_row_store(&a, &addr);
    let err_path = scratch.dir.path().join("datahub.stderr");
    let err = std::fs::File::create(&err_path).expect("the daemon's stderr file");
    let mut daemon = vike_log::reaper::Reaper(
        scratch
            .tool()
            .env_remove("RUST_LOG")
            .env_remove("VIKE_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err)
            .spawn()
            .expect("spawn the datahub"),
    );

    let log = || std::fs::read_to_string(&err_path).unwrap_or_default();
    let listening =
        |log: &str| log.lines().find(|l| l.contains("listening addr=")).map(str::to_string);
    let up = vike_log::poll::wait_until(Duration::from_secs(60), || {
        listening(&log()).is_some() || daemon.0.try_wait().ok().flatten().is_some()
    });
    let log = log();
    let line = listening(&log).unwrap_or_else(|| panic!("no datahub at {addr} (up: {up}): {log}"));
    assert!(line.contains(&format!("store={}", a.display())), "serves the row's tree: {log}");
}
