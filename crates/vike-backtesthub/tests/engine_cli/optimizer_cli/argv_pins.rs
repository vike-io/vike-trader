//! The POSITIVE argv pins of decision 0155 for this crate's `backtesthub` binary and its research bins:
//! every command line an operator, a deployed unit or `vike-cli`'s spawn could write BEFORE `clap`
//! read it must still be ACCEPTED, with the same meaning, AFTER.
//!
//! ⚠ **Written against the hand-rolled readers FIRST and run unchanged against `clap`'s.** A case
//! here therefore asserts only what is true of both: a marker that proves the argv got PAST the
//! reader (an error that can only come from the step after it, such as `failed to load profile`
//! for a profile that does not exist), never the wording of a usage refusal, which `clap` owns now.
//! A refusal that exists to be refused stays in `flags.rs` and its siblings.
//!
//! ⚠ **The deployed spellings are in here by name**: `backtesthub --addr` (a flag with NO value, last
//! on the line, `deploy/vike-backtesthub.service`'s `ExecStart=`), `--addr HOST:PORT`,
//! `--addr=HOST:PORT`, `--named-run`, and the argv `vike-cli` spawns the engine with
//! (`crates/vike-cli/src/cmd/backtest/execute.rs`'s `execute_local` and the `data` engine argv
//! builders under `crates/vike-cli/src/cmd/data/hist/`).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::*;

// ---------------------------------------------------------------------------- the run verb

/// The marker of an argv the reader ACCEPTED and the profile load then failed on: the file does not
/// exist. It is printed by the step AFTER every flag has been read and every search rule applied,
/// so it is present exactly when the whole command line was accepted.
const GOT_PAST_THE_FLAGS: &str = "failed to load profile";

fn accepted_up_to_the_profile_load(args: &[&str]) {
    let out = run(args);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "`backtest {args:?}` stderr: {stderr:?}");
    assert!(
        stderr.contains(GOT_PAST_THE_FLAGS),
        "`backtest {args:?}` must be ACCEPTED by the argv reader and fail only on the profile \
         file that does not exist; stderr: {stderr:?}"
    );
}

/// Every spelling of the profile and of every search flag, spaced and inline, before and after the
/// positional. The profile `nope.toml` does not exist, so acceptance is visible without a store.
#[test]
fn every_run_spelling_an_operator_wrote_is_still_accepted() {
    let cases: &[&[&str]] = &[
        &["nope.toml"],
        &["--profile", "nope.toml"],
        &["--profile=nope.toml"],
        &["nope.toml", "--json"],
        &["--json", "nope.toml"],
        &["nope.toml", "--rank-by", "return"],
        &["nope.toml", "--rank-by=return"],
        &["nope.toml", "--rank-by", "SHARPE"],
        &["--rank-by", "max_dd", "nope.toml"],
        &["nope.toml", "--rank-by", "multi", "--json"],
        &["nope.toml", "--optimizer", "grid"],
        &["nope.toml", "--optimizer", "tpe", "--trials", "8", "--seed", "3"],
        &["nope.toml", "--optimizer=tpe", "--trials=8", "--seed=3"],
        &["nope.toml", "--optimizer", "TPE"],
        &["--optimizer", "tpe", "nope.toml"],
        &["nope.toml", "--optimizer", "euler", "--euler-depth", "2"],
        &["nope.toml", "--optimizer=euler", "--euler-depth=2"],
        &["nope.toml", "--optimizer", "genetic", "--seed", "7"],
        &["nope.toml", "--keep-trials", "returns"],
        &["nope.toml", "--keep-trials=none"],
        &["nope.toml", "--keep-trials", "scalars", "--resume", "20260101T000000-abc"],
        &["nope.toml", "--min-trades", "50"],
        &["nope.toml", "--min-trades=0"],
        &["nope.toml", "--progress", "json"],
        &["nope.toml", "--progress=none"],
        // The argv `vike-cli backtest run --local` spawns: `--profile` first, the search flags
        // spelled as themselves, then `--json`.
        &[
            "--profile",
            "nope.toml",
            "--rank-by",
            "return",
            "--optimizer",
            "tpe",
            "--trials",
            "9",
            "--seed",
            "1",
            "--json",
        ],
        // A flag that only means something to the daemon, written beside a profile: ignored, never
        // refused.
        &["nope.toml", "--named-run"],
        // `--archive` is a VALUED flag: its value is not a second profile.
        &["nope.toml", "--archive", "some_dir"],
        &["--archive=some_dir", "nope.toml"],
        &["--archive", "some_dir", "nope.toml"],
    ];
    for args in cases {
        accepted_up_to_the_profile_load(args);
    }
}

/// `--list` and `--list-optimizers [--json]` answer without a store or a profile.
#[test]
fn the_listing_flags_are_still_accepted() {
    let out = run(&["--list"]);
    assert!(out.status.success(), "stderr: {:?}", stderr_of(&out));
    assert!(!out.stdout.is_empty());

    let out = run(&["--list-optimizers"]);
    assert!(out.status.success(), "stderr: {:?}", stderr_of(&out));
    let plain = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(plain.contains("tpe"), "{plain:?}");

    let out = run(&["--list-optimizers", "--json"]);
    assert!(out.status.success(), "stderr: {:?}", stderr_of(&out));
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json prints one JSON document");
    assert!(json.is_object(), "{json}");
}

/// Help and version, every spelling, answer on STDOUT with exit 0 and an empty STDERR.
#[test]
fn help_and_version_answer_on_stdout_in_every_spelling() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", stderr_of(&out));
        assert!(stderr_of(&out).trim().is_empty(), "{flag}: {:?}", stderr_of(&out));
        let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(stdout.contains("usage"), "{flag}: {stdout:?}");
        assert!(stdout.contains("--optimizer"), "{flag}: {stdout:?}");
    }
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", stderr_of(&out));
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(stdout.starts_with("backtesthub "), "{flag}: {stdout:?}");
        assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{flag}: {stdout:?}");
    }
}

/// `--store` on the run path is REFUSED BY NAME, in both spellings the old reader saw.
#[test]
fn the_store_flag_on_the_run_path_is_still_refused_by_name() {
    let (_dir, profile, store) = scratch(SINGLE_PROFILE);
    for args in
        [vec![profile.as_str(), "--store", store.as_str()], vec![profile.as_str(), "--store=x"]]
    {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr:?}");
        assert!(stderr.contains("no longer reads history locally"), "{args:?}: {stderr:?}");
        assert!(!Path::new(&store).exists(), "a refusal opens nothing: {store}");
    }
}

/// The retired spellings keep answering BY NAME when a script still passes their old companions
/// (`--days`, `--out`, `--kind`, ...): the cleanup scripts are the ones nobody watches.
#[test]
fn a_retired_flag_with_its_old_companions_is_still_refused_by_name() {
    for args in [
        vec!["--fetch", "binance:BTCUSDT:1h", "--days", "180"],
        vec!["--fetch=binance:BTCUSDT:1h", "--days=180"],
        vec!["--fetch-starter"],
        vec!["--seed-demo"],
        vec!["--export", "demo:BTCUSDT:1h", "--out", "x.parquet", "--from", "2025-01-01"],
        vec!["--rm-series", "--kind", "bar", "--venue", "binance", "--produced-by", "x:", "--yes"],
        vec!["--rm-series", "--kind=bar", "--venue=binance", "--dry-run"],
    ] {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(stderr.contains("is retired"), "{args:?}: {stderr:?}");
    }
    for args in
        [vec!["x.toml", "--search", "tpe"], vec!["x.toml", "--search=tpe"], vec!["--search"]]
    {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(stderr.contains("--search is retired"), "{args:?}: {stderr:?}");
    }
}

// ---------------------------------------------------------------------------- the daemon

/// Spawn `backtest` as the daemon with the given argv, wait for `needle` on its stderr, then kill
/// it. The scratch project's `config.backtest_addr` row is `row` (the address a BARE `--addr`
/// serves on), so the test never binds the box's real compute port.
fn daemon_says(args: &[&str], row: &str, needle: &str) {
    let scratch = engine::Scratch::project();
    vike_secrets::plant_settings_rows(
        &scratch.path().join(vike_paths::state_path::PROJECT_SETTINGS_DIR),
        &vike_secrets::StoredSettings {
            settings: vec![
                vike_secrets::SettingRow {
                    section: "config".to_string(),
                    key: "datahub_addr".to_string(),
                    value: serde_json::Value::String(NO_DATAHUB.to_string()).to_string(),
                },
                vike_secrets::SettingRow {
                    section: "config".to_string(),
                    key: "backtest_addr".to_string(),
                    value: serde_json::Value::String(row.to_string()).to_string(),
                },
                vike_secrets::SettingRow {
                    section: "preferences".to_string(),
                    key: "log_file_level".to_string(),
                    value: "\"off\"".to_string(),
                },
            ],
            ..Default::default()
        },
    )
    .expect("plant the daemon's settings");
    let mut child = scratch
        .engine()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn backtest {args:?}: {e}"));
    let stderr = child.stderr.take().expect("a piped stderr");
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let found = loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(line) => {
                let hit = line.contains(needle);
                seen.push(line);
                if hit {
                    break true;
                }
            }
            Err(_) => break false,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(found, "`backtest {args:?}` must print {needle:?}; stderr: {seen:#?}");
}

/// The deployed unit is `backtesthub --addr` with NOTHING after it; the other spellings are the ones
/// an operator types. Every one becomes the daemon, on the address the line (or the row) names.
#[test]
fn every_addr_spelling_becomes_the_daemon() {
    // BARE and last: the address is the `config.backtest_addr` row.
    daemon_says(&["--addr"], "127.0.0.1:0", "listening on 127.0.0.1:0");
    // BARE, followed by another flag: still no value.
    daemon_says(&["--addr", "--named-run"], "127.0.0.1:0", "listening on 127.0.0.1:0");
    daemon_says(&["--named-run", "--addr"], "127.0.0.1:0", "listening on 127.0.0.1:0");
    // A value: spaced and inline, and the row loses.
    daemon_says(&["--addr", "127.0.0.1:0"], "127.0.0.1:1", "listening on 127.0.0.1:0");
    daemon_says(&["--addr=127.0.0.1:0"], "127.0.0.1:1", "listening on 127.0.0.1:0");
    daemon_says(&["--named-run", "--addr", "127.0.0.1:0"], "127.0.0.1:1", "listening on");
}

/// A blank `--addr` is refused, never read as "the configured address".
#[test]
fn a_blank_addr_is_still_refused() {
    for args in [vec!["--addr", ""], vec!["--addr="]] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {:?}", stderr_of(&out));
        assert!(!stderr_of(&out).contains("listening on"), "{args:?}");
    }
}

// ---------------------------------------------------------------------------- `data` verbs

#[test]
fn the_data_verb_answers_help_on_stdout_with_exit_zero() {
    for args in [vec!["data"], vec!["data", "--help"], vec!["data", "-h"], vec!["data", "help"]] {
        let out = run(&args);
        assert!(out.status.success(), "{args:?}: {:?}", stderr_of(&out));
        let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(stdout.contains("usage"), "{args:?}: {stdout:?}");
        assert!(stderr_of(&out).trim().is_empty(), "{args:?}: {:?}", stderr_of(&out));
    }
}

/// The offline writers, in the spellings `vike-cli data hist` spawns (`--store` last, the selector
/// flags spaced) and the inline ones an operator may type.
#[test]
fn the_offline_data_verbs_accept_every_spelling_they_accepted() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    let store = store.to_str().expect("a UTF-8 path");

    let out = run(&["data", "seed-demo", "--store", store]);
    assert!(out.status.success(), "seed-demo: {:?}", stderr_of(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("seeded SYNTHETIC"));

    // A second store through the inline spelling.
    let inline = dir.path().join("hist_inline");
    let inline_flag = format!("--store={}", inline.display());
    let out = run(&["data", "seed-demo", inline_flag.as_str()]);
    assert!(out.status.success(), "seed-demo --store=: {:?}", stderr_of(&out));

    // `rm --dry-run` over what was seeded: the plan prints and nothing is deleted.
    for args in [
        vec![
            "data",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "demo",
            "--symbol",
            "BTCUSDT",
            "--interval",
            "1h",
            "--dry-run",
            "--store",
            store,
        ],
        vec![
            "data",
            "rm",
            "--kind=bar",
            "--venue=demo",
            "--symbol=BTCUSDT",
            "--interval=1h",
            "--dry-run",
            "--json",
            "--store",
            store,
        ],
    ] {
        let out = run(&args);
        assert!(out.status.success(), "{args:?}: {:?}", stderr_of(&out));
    }

    // `repair` rehearses by default (`vike-cli data hist repair` forwards `--dry-run` or `--yes`).
    let out = run(&[
        "data",
        "repair",
        "--kind",
        "bar",
        "--venue",
        "demo",
        "--symbol",
        "BTCUSDT",
        "--interval",
        "1h",
        "--dry-run",
        "--json",
        "--store",
        store,
    ]);
    assert!(out.status.success(), "repair: {:?}", stderr_of(&out));
    let out = run(&[
        "data",
        "repair",
        "--kind=bar",
        "--venue=demo",
        "--symbol=BTCUSDT",
        "--interval=1h",
        "--store",
        store,
    ]);
    assert!(out.status.success(), "repair, inline: {:?}", stderr_of(&out));
}

/// `export` reads through a datahub, so with none listening it fails AFTER its argv was read —
/// at the read, with the spec named. Every spelling of its flags reaches that point.
#[test]
fn data_export_accepts_every_spelling_up_to_the_datahub_read() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let out_file = dir.path().join("x.parquet");
    let out_file = out_file.to_str().expect("a UTF-8 path");
    let inline_out = format!("--out={out_file}");
    for args in [
        vec!["data", "export", "demo:BTCUSDT:1h", "--out", out_file],
        vec!["data", "export", "demo:BTCUSDT:1h", inline_out.as_str()],
        vec![
            "data",
            "export",
            "demo:BTCUSDT:1h",
            "--out",
            out_file,
            "--from",
            "2025-01-01T00",
            "--to",
            "2025-02-01T00",
        ],
        vec!["data", "export", "--out", out_file, "demo:BTCUSDT:1h", "--from=2025-01-01T00"],
    ] {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(
            stderr.contains("reading demo:BTCUSDT:1h from"),
            "{args:?} must be accepted and fail only at the datahub read; stderr: {stderr:?}"
        );
    }
    // `--store` on the reader is refused BY NAME, both spellings.
    for args in [
        vec!["data", "export", "demo:BTCUSDT:1h", "--out", out_file, "--store", "/srv/hist"],
        vec!["data", "export", "demo:BTCUSDT:1h", "--out", out_file, "--store=/srv/hist"],
    ] {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(stderr.contains("no longer reads history locally"), "{args:?}: {stderr:?}");
    }
}

/// `data fetch` is retired and says so; its spec and `--days` are still read (a negative number is
/// a value).
#[test]
fn data_fetch_is_still_refused_by_name_with_its_old_flags() {
    for args in [
        vec!["data", "fetch", "binance:BTCUSDT:1h", "--days", "180"],
        vec!["data", "fetch", "binance:BTCUSDT:1h", "--days=180", "--store", "x"],
        vec!["data", "fetch", "nope:NOPE:99z", "--days", "-4"],
        vec!["data", "fetch", "binance:BTCUSDT:1h", "--from", "2025-01-01", "--to", "2025-02-01"],
    ] {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(stderr.contains("RETIRED"), "{args:?}: {stderr:?}");
    }
}

// ---------------------------------------------------------------------------- `trials`

/// `trials <id>` with every flag it takes, spaced and inline: the run does not exist, so the
/// refusal is the lookup's — proof the argv was read.
#[test]
fn trials_accepts_every_spelling_up_to_the_run_lookup() {
    for args in [
        vec!["trials", "nope-id"],
        vec!["trials", "nope-id", "--sort", "sharpe", "--top", "3"],
        vec!["trials", "nope-id", "--sort=max_dd", "--top=3", "--json"],
        vec!["trials", "nope-id", "--export-params", "p.toml"],
        vec!["trials", "nope-id", "--export-params=p.toml", "--sort", "SCORE"],
    ] {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(stderr.contains("no run \"nope-id\""), "{args:?}: {stderr:?}");
    }
    let out = run(&["trials", "--help"]);
    assert!(out.status.success(), "{:?}", stderr_of(&out));
    assert!(String::from_utf8_lossy(&out.stdout).to_lowercase().contains("usage"));
}

// ---------------------------------------------------------------------------- the research bins

/// A bin spawned in a scratch directory with `args`; its stderr.
fn bin_stderr(exe: &str, args: &[&str]) -> (Option<i32>, String) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let out = Command::new(exe)
        .current_dir(dir.path())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run {exe} {args:?}: {e}"));
    (out.status.code(), String::from_utf8_lossy(&out.stderr).to_string())
}

/// `cheap_np_askgate` reads its argv, then its entries file: a file that does not exist is the
/// marker. Every flag it takes is on the first line.
#[test]
fn cheap_np_askgate_accepts_every_flag_it_took() {
    let exe = env!("CARGO_BIN_EXE_cheap_np_askgate");
    for args in [
        vec![
            "--entries",
            "nope.csv",
            "--venue",
            "polymarket",
            "--theta",
            "0.05",
            "--size",
            "2",
            "--delay-ms",
            "100",
            "--live-ms",
            "1,2",
            "--out",
            "o.csv",
            "--json",
        ],
        vec!["--entries=nope.csv", "--theta=0.05", "--live-ms=1,2"],
    ] {
        let (code, stderr) = bin_stderr(exe, &args);
        assert!(code.is_some_and(|c| c != 0), "{args:?}: {stderr:?}");
        assert!(stderr.contains("--entries:"), "{args:?} must reach the entries read: {stderr:?}");
    }
    let (code, stderr) = bin_stderr(exe, &["--entries", "nope.csv", "--store", "x"]);
    assert!(code.is_some_and(|c| c != 0), "{stderr:?}");
    assert!(stderr.contains("no longer reads history locally"), "{stderr:?}");
    // No `--entries` is refused and the refusal names it.
    let (code, stderr) = bin_stderr(exe, &["--json"]);
    assert!(code.is_some_and(|c| c != 0), "{stderr:?}");
    assert!(stderr.contains("--entries"), "{stderr:?}");
}

#[test]
fn cheap_np_depth_accepts_every_flag_it_took() {
    let exe = env!("CARGO_BIN_EXE_cheap_np_depth");
    for args in [
        vec![
            "--entries",
            "nope.csv",
            "--venue",
            "polymarket",
            "--sizes",
            "10,50",
            "--theta",
            "0.05",
            "--order",
            "local_ts",
            "--consume-trades",
            "--out",
            "o.csv",
            "--json",
        ],
        vec!["--entries=nope.csv", "--sizes=10,50", "--order=local_ts"],
    ] {
        let (code, stderr) = bin_stderr(exe, &args);
        assert!(code.is_some_and(|c| c != 0), "{args:?}: {stderr:?}");
        assert!(stderr.contains("--entries:"), "{args:?} must reach the entries read: {stderr:?}");
    }
    let (code, stderr) = bin_stderr(exe, &["--entries", "nope.csv", "--store=x"]);
    assert!(code.is_some_and(|c| c != 0), "{stderr:?}");
    assert!(stderr.contains("no longer reads history locally"), "{stderr:?}");
}

#[test]
fn cheap_np_run_accepts_every_flag_it_took() {
    let exe = env!("CARGO_BIN_EXE_cheap_np_run");
    for args in [
        vec![
            "--from",
            "2025-01-01",
            "--to",
            "2025-01-02",
            "--resolutions",
            "nope.csv",
            "--poly-venue",
            "polymarket",
            "--spot-venue",
            "spot",
            "--spot-symbol",
            "BTCUSDT",
            "--mode",
            "flip",
            "--theta",
            "0.05",
            "--size",
            "1.0",
            "--sigma-scale",
            "1.0",
            "--max-windows",
            "3",
            "--signals-out",
            "s.csv",
            "--latency-ms",
            "0,1000",
            "--json",
        ],
        vec!["--from=2025-01-01", "--to=2025-01-02", "--resolutions=nope.csv", "--mode=hold"],
    ] {
        let (code, stderr) = bin_stderr(exe, &args);
        assert!(code.is_some_and(|c| c != 0), "{args:?}: {stderr:?}");
        assert!(
            stderr.contains("--resolutions:"),
            "{args:?} must reach the resolutions read: {stderr:?}"
        );
    }
    let (code, stderr) = bin_stderr(
        exe,
        &["--from", "2025-01-01", "--to", "2025-01-02", "--resolutions", "n.csv", "--store", "x"],
    );
    assert!(code.is_some_and(|c| c != 0), "{stderr:?}");
    assert!(stderr.contains("no longer reads history locally"), "{stderr:?}");
}

#[test]
fn sport_taker_run_accepts_every_flag_it_took() {
    let exe = env!("CARGO_BIN_EXE_sport_taker_run");
    for args in [
        vec![
            "--wallet-buys",
            "nope.tsv",
            "--scan-only",
            "--triggers-out",
            "t.tsv",
            "--tape",
            "tape.tsv",
            "--resolutions",
            "res.tsv",
            "--delays-ms",
            "0,5000",
            "--bets-out",
            "b.tsv",
            "--holds",
            "h.tsv",
            "--exclude-self",
            "--no-maker-fills",
            "--conviction-usd",
            "100",
            "--from",
            "2025-01-01",
            "--to",
            "2025-01-02",
            "--json",
        ],
        vec!["--wallet-buys=nope.tsv", "--conviction-usd=100", "--from=2025-01-01"],
    ] {
        let (code, stderr) = bin_stderr(exe, &args);
        assert!(code.is_some_and(|c| c != 0), "{args:?}: {stderr:?}");
        assert!(
            stderr.contains("--wallet-buys:"),
            "{args:?} must reach the wallet-buys read: {stderr:?}"
        );
    }
}
