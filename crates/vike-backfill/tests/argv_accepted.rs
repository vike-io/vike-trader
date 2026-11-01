//! **Every command line an operator or a deployed unit could write for a `vike-backfill` bin and
//! have ACCEPTED stays accepted** (decision 0155 verdict 6), pinned over the SHIPPED binaries.
//!
//! Written against the hand-rolled argv code (the `CliSpec::triage` gate plus one-flag `arg` lookups) BEFORE the
//! bins moved to `clap`, run there, and run again unchanged after: a process boundary is the only
//! seam both generations share.
//!
//! ## How "accepted" is observed without doing the work
//!
//! A bin that accepts its argv goes on to do something, and the something must be harmless here. So
//! each case carries an argument that makes the run stop LOCALLY, after the argv was read and before
//! any network call or store write, with an exit code and a message that belong to the PROGRAM and
//! not to the argv reader: an unknown `--schema`, a `--start` that is not a date, a `--dry-run`
//! that plans and stops, a `--file` that does not exist, an absent credential (the sandbox below has
//! no store, so the live gate refuses). A usage error from the argv reader exits 2 with different
//! words, so the exit code plus the program's own sentence discriminate. The sentences are the
//! post-parse ones the migration keeps; if a migration reworded one, the case says so by failing.
//!
//! ## The sandbox
//!
//! The child gets an EMPTY environment but `VIKE_SETTINGS_DIR`, a temp project as its working
//! directory, and `--store` inside it, so no case reads a real credential store, a real hist store
//! or a log directory of the checkout.
//!
//! The deployed line is `deploy/vike-cohort-collector@.service`'s `ExecStart=`:
//! `vikedata_backfill --asset %i --schedule --catch-up 3 --store <dir>`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(60);

/// One flag of a case: `Some(value)` for a valued flag, `None` for a switch.
type Flag = (&'static str, Option<String>);

struct Case {
    exe: &'static str,
    bin: &'static str,
    /// Where the argv comes from, for a failure message.
    origin: &'static str,
    flags: Vec<Flag>,
    code: i32,
    /// A substring of stdout + stderr that only the PROGRAM (not the argv reader) prints.
    said: &'static str,
}

struct Sandbox {
    _dir: tempfile::TempDir,
    root: PathBuf,
    settings: PathBuf,
    store: PathBuf,
    tokens_file: PathBuf,
    missing_file: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temp project");
        let root = dir.path().to_path_buf();
        let settings = root.join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        let tokens_file = root.join("tokens.txt");
        std::fs::write(&tokens_file, "# ids\ntoken_a\n\ntoken_b\n").expect("tokens file");
        Self {
            store: root.join("hist"),
            missing_file: root.join("does-not-exist.json"),
            _dir: dir,
            root,
            settings,
            tokens_file,
        }
    }
}

fn s(path: &Path) -> String {
    path.to_str().expect("utf-8 temp path").to_owned()
}

fn v(text: &str) -> Option<String> {
    Some(text.to_owned())
}

fn run(case: &Case, sandbox: &Sandbox, args: &[String]) -> Output {
    let mut child = Command::new(case.exe)
        .args(args)
        .env_clear()
        // The name through its constant, not a literal: a literal here is a sighting the settings
        // registry scores against the `vike-backfill` row (`Naming::Literal` there), and a
        // `Command::env` is no read.
        .env(vike_secrets::SETTINGS_DIR_ENV, &sandbox.settings)
        .current_dir(&sandbox.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {} {args:?}: {e}", case.bin));
    let deadline = Instant::now() + DEADLINE;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`{} {}` ({}) did not exit within {DEADLINE:?}: it is doing real work, which a \
                     case in this file must never reach",
                    case.bin,
                    args.join(" "),
                    case.origin
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    child.wait_with_output().expect("collect the exited child's output")
}

/// `--flag value` for a valued flag, `--flag` for a switch.
fn spaced(flags: &[Flag]) -> Vec<String> {
    flags
        .iter()
        .flat_map(|(flag, value)| {
            std::iter::once((*flag).to_owned()).chain(value.iter().cloned()).collect::<Vec<_>>()
        })
        .collect()
}

/// `--flag=value` for a valued flag, `--flag` for a switch.
fn equals(flags: &[Flag]) -> Vec<String> {
    flags
        .iter()
        .map(|(flag, value)| match value {
            Some(value) => format!("{flag}={value}"),
            None => (*flag).to_owned(),
        })
        .collect()
}

fn judge(case: &Case, sandbox: &Sandbox, args: &[String]) {
    let out = run(case, sandbox, args);
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        out.status.code(),
        Some(case.code),
        "`{} {}` ({}) must be ACCEPTED and then stop on the program's own rule with exit {}; \
         output: {text}",
        case.bin,
        args.join(" "),
        case.origin,
        case.code
    );
    assert!(
        text.contains(case.said),
        "`{} {}` ({}) must stop on {:?}, not on the argv reader; output: {text}",
        case.bin,
        args.join(" "),
        case.origin,
        case.said
    );
}

fn cases(sb: &Sandbox) -> Vec<Case> {
    let store = s(&sb.store);
    let tokens = s(&sb.tokens_file);
    let missing = s(&sb.missing_file);
    #[allow(unused_mut, clippy::allow_attributes)]
    let mut all: Vec<Case> = vec![
        Case {
            exe: env!("CARGO_BIN_EXE_eod_backfill"),
            bin: "eod_backfill",
            origin: "bin docs: --source --symbols --years --venue --store",
            flags: vec![
                ("--source", v("nosuch")),
                ("--symbols", v("SPX=^GSPC,VIX=^VIX,AAPL")),
                ("--years", v("3")),
                ("--venue", v("index")),
                ("--store", Some(store.clone())),
            ],
            code: 2,
            said: "unknown --source 'nosuch'",
        },
        Case {
            exe: env!("CARGO_BIN_EXE_exec_trade_backfill"),
            bin: "exec_trade_backfill",
            origin: "bin docs: --file --symbol --venue --store",
            flags: vec![
                ("--file", Some(missing.clone())),
                ("--symbol", v("BTCUSDT")),
                ("--venue", v("binance")),
                ("--store", Some(store.clone())),
            ],
            code: 1,
            said: "reading",
        },
        Case {
            exe: env!("CARGO_BIN_EXE_pmxt_backfill"),
            bin: "pmxt_backfill",
            origin: "bin docs: every flag, the --dry-run rehearsal",
            flags: vec![
                ("--from", v("2026-01-01T00")),
                ("--to", v("2026-01-01T02")),
                ("--tokens", v("t1,t2")),
                ("--tokens-file", Some(tokens.clone())),
                ("--store", Some(store.clone())),
                ("--rate-ms", v("5")),
                ("--dry-run", None),
            ],
            code: 0,
            said: "",
        },
    ];

    #[cfg(feature = "databento")]
    all.push(Case {
        exe: env!("CARGO_BIN_EXE_databento_backfill"),
        bin: "databento_backfill",
        origin: "bin docs: --dataset --symbol --schema --start --end --store --venue",
        flags: vec![
            ("--dataset", v("GLBX.MDP3")),
            ("--symbol", v("ESZ4")),
            ("--schema", v("bogus")),
            ("--start", v("2024-01-02T00:00:00")),
            ("--end", v("2024-01-02T00:01:00")),
            ("--store", Some(store.clone())),
            ("--venue", v("databento")),
        ],
        code: 1,
        said: "unknown schema 'bogus'",
    });

    #[cfg(feature = "tardis")]
    all.push(Case {
        exe: env!("CARGO_BIN_EXE_tardis_backfill"),
        bin: "tardis_backfill",
        origin: "bin docs: every flag, bars with --interval",
        flags: vec![
            ("--exchange", v("deribit")),
            ("--symbol", v("BTC-PERPETUAL")),
            ("--data-type", v("bars")),
            ("--start", v("2024-13-45")),
            ("--end", v("2024-03-03")),
            ("--store", Some(store.clone())),
            ("--venue", v("tardis")),
            ("--interval", v("5m")),
        ],
        code: 1,
        said: "--start/--end must be YYYY-MM-DD",
    });

    #[cfg(feature = "vikedata")]
    {
        // The deployed unit's ExecStart, byte for byte apart from the store path.
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vikedata_backfill"),
            bin: "vikedata_backfill",
            origin: "deploy/vike-cohort-collector@.service ExecStart=",
            flags: vec![
                ("--asset", v("BTC")),
                ("--schedule", None),
                ("--catch-up", v("3")),
                ("--store", Some(store.clone())),
            ],
            code: 1,
            said: "is not in the credential store",
        });
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vikedata_backfill"),
            bin: "vikedata_backfill",
            origin: "bin docs: periodic --days",
            flags: vec![
                ("--asset", v("BTC")),
                ("--axis", v("size")),
                ("--days", v("30")),
                ("--exchange", v("hyperliquid")),
                ("--api-base", v("http://127.0.0.1:9/v1")),
                ("--store", Some(store.clone())),
            ],
            code: 1,
            said: "is not in the credential store",
        });
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vikedata_backfill"),
            bin: "vikedata_backfill",
            origin: "bin docs: ad-hoc --axis --grading --start --end",
            flags: vec![
                ("--asset", v("BTC")),
                ("--axis", v("pnl")),
                ("--grading", v("unrealized")),
                ("--start", v("2026-04-01")),
                ("--end", v("2026-04-08")),
                ("--store", Some(store.clone())),
            ],
            code: 1,
            said: "is not in the credential store",
        });
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vikedata_backfill"),
            bin: "vikedata_backfill",
            origin: "bin usage: --panel --start --end",
            flags: vec![
                ("--asset", v("BTC")),
                ("--panel", None),
                ("--start", v("2026-04-01")),
                ("--end", v("2026-04-08")),
                ("--store", Some(store.clone())),
            ],
            code: 1,
            said: "is not in the credential store",
        });
        // A program rule, reached only because the argv was read: still exit 2, still its own words.
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vikedata_backfill"),
            bin: "vikedata_backfill",
            origin: "the schedule's safety refusal",
            flags: vec![("--asset", v("BTC")), ("--schedule", None), ("--days", v("30"))],
            code: 2,
            said: "two different run shapes",
        });
    }

    #[cfg(feature = "vike-archive")]
    {
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_events_api_backfill"),
            bin: "events_api_backfill",
            origin: "bin docs: every flag, the --dry-run plan",
            flags: vec![
                ("--from", v("2026-01-01")),
                ("--to", v("2026-01-02")),
                ("--tokens-file", Some(tokens.clone())),
                ("--tokens", v("token_c")),
                ("--store", Some(store.clone())),
                ("--base", v("http://127.0.0.1:9")),
                ("--kind", v("book")),
                ("--concurrency", v("2")),
                ("--dry-run", None),
            ],
            code: 0,
            said: "dry-run: 2 day(s)",
        });
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vike_archive_backfill"),
            bin: "vike_archive_backfill",
            origin: "bin docs: the whole flag set; --file with --dry-run is the program's refusal",
            flags: vec![
                ("--from", v("2026-07-28")),
                ("--to", v("2026-07-28")),
                ("--kind", v("book")),
                ("--tokens", v("a,b")),
                ("--tokens-file", Some(tokens.clone())),
                ("--store", Some(store.clone())),
                ("--base", v("http://127.0.0.1:9/archive")),
                ("--dry-run", None),
                ("--asset", v("btc")),
                ("--tenor", v("5m")),
                ("--discovery-base", v("http://127.0.0.1:9")),
                ("--bulk", None),
                ("--bulk-max-batches", v("3")),
                ("--bulk-max-rows", v("4")),
                ("--bulk-max-bytes", v("5")),
                ("--file", Some(missing.clone())),
                ("--jobs", v("2")),
            ],
            code: 1,
            said: "--file and --dry-run are mutually exclusive",
        });
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vike_archive_backfill"),
            bin: "vike_archive_backfill",
            origin: "bin docs: --gaps (the store's own coverage report)",
            flags: vec![("--gaps", None), ("--store", Some(store.clone()))],
            code: 0,
            said: "--gaps:",
        });
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vike_archive_backfill"),
            bin: "vike_archive_backfill",
            origin: "bin docs: --file local ingest with --bulk and --jobs",
            flags: vec![
                ("--from", v("2026-07-28")),
                ("--to", v("2026-07-28")),
                ("--kind", v("book")),
                ("--file", Some(missing.clone())),
                ("--store", Some(store.clone())),
                ("--tokens", v("a")),
                ("--bulk", None),
                ("--bulk-max-batches", v("3")),
                ("--bulk-max-rows", v("4")),
                ("--bulk-max-bytes", v("5")),
                ("--jobs", v("2")),
            ],
            code: 1,
            said: "local-file ingest failed",
        });
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vike_archive_backfill"),
            bin: "vike_archive_backfill",
            origin: "bin docs: --file local ingest, serial",
            flags: vec![
                ("--from", v("2026-07-28")),
                ("--to", v("2026-07-28")),
                ("--kind", v("trade")),
                ("--file", Some(missing.clone())),
                ("--store", Some(store.clone())),
                ("--jobs", v("1")),
            ],
            code: 1,
            said: "local-file ingest failed",
        });
        // A program rule reached only because the argv was read.
        all.push(Case {
            exe: env!("CARGO_BIN_EXE_vike_archive_backfill"),
            bin: "vike_archive_backfill",
            origin: "bin docs: --asset and --tenor go together",
            flags: vec![
                ("--from", v("2026-07-28")),
                ("--to", v("2026-07-28")),
                ("--asset", v("btc")),
            ],
            code: 2,
            said: "--asset given without --tenor",
        });
    }

    all
}

/// The spelling every deployed unit and every doc uses: `--flag value`.
#[test]
fn every_deployed_and_documented_argv_is_accepted() {
    let sandbox = Sandbox::new();
    let cases = cases(&sandbox);
    assert!(cases.len() >= 3, "the table must not go empty");
    for case in &cases {
        judge(case, &sandbox, &spaced(&case.flags));
    }
}

/// Decision 0155 verdict 3: `--flag=value` is read everywhere. The hand-rolled bins REFUSED it
/// (`CliSpec::triage` called it an unknown argument), so this test is the one that is red on the old
/// code and green after the migration; the test above is green on both.
#[test]
fn the_equals_spelling_reads_the_same_as_the_spaced_one() {
    let sandbox = Sandbox::new();
    for case in &cases(&sandbox) {
        judge(case, &sandbox, &equals(&case.flags));
    }
}
