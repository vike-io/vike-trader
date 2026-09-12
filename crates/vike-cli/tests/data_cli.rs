//! `vike-cli data` — the store-filling AND store-inspecting verb, driven as the SHIPPED binary.
//!
//! The grammar is unit-tested beside the module; what needs a real process is the part no parser
//! test can see: which exit code reaches a caller, that the WRITE half genuinely SPAWNS an engine
//! rather than pretending to, and that the READ half genuinely talks to a datahub.
//!
//! The read half's cases run against a REAL [`vike_datahub::serve`] over a seeded in-memory
//! `MemHistStore` on an ephemeral loopback port — the spawn pattern `tests/backtest_cli.rs` and
//! `crates/vike-datahub/tests/roundtrip.rs` already use, with a store that actually holds rows so
//! the inventory has something to enumerate. No prod store, no external network, no DataFusion:
//! the double is the trait-only in-memory one, which is what keeps this file on the fast lane
//! beside the crate it tests.
//!
//! ⚠ **Every case names the engine with `--engine`, and that is not laziness.** The search's third
//! rung looks beside THIS executable, and a lane that has also built
//! `-p vike-backtest --features datafusion-store` into the same `target/` really does leave a
//! `backtest` binary sitting there. A test that relied on the search finding nothing would pass on
//! a developer box and fail in the full CI matrix, for a reason having nothing to do with this
//! code — so the tests that care about the MISS name a path that is not there, and the tests that
//! care about the SPAWN name a stand-in they wrote themselves.
//!
//! The child's environment is pinned on the child (`Command::env` / `env_remove`, never
//! `std::env::set_var`): without the settings redirect a run on a developer box resolves the REPO's
//! settings directory, and an exported removed variable would make every case exit on the startup
//! refusal instead of the rung it is testing.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_model::{Bar, SymbolProperties};

fn run(settings_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", settings_dir)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The verb's help is the only place its subcommands are named — it takes no default action — so a
/// help that did not list every one would leave "get some market data" (or "see what data is
/// there") undiscoverable.
#[test]
fn help_names_every_subcommand_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for sub in ["fetch", "fetch-starter", "seed-demo", "export", "list", "coverage", "rm"] {
        assert!(text.contains(sub), "`data --help` must list `{sub}`: {text}");
    }
}

/// A command line the user can fix exits on the USAGE rung, and every one of these is caught HERE —
/// before a process is spawned, so the diagnostic comes from the binary they typed.
#[test]
fn a_bad_command_line_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data"], "subcommand"),
        (vec!["data", "frobnicate"], "unknown `data` subcommand"),
        (vec!["data", "fetch"], "VENUE:SYMBOL:INTERVAL"),
        (vec!["data", "fetch", "binance:BTCUSDT"], "VENUE:SYMBOL:INTERVAL"),
        (vec!["data", "fetch", "binance:BTCUSDT:1h"], "--days"),
        (vec!["data", "fetch", "binance:BTCUSDT:1h", "--days", "x"], "--days"),
        (vec!["data", "fetch", "binance:BTCUSDT:1h", "--from", "0"], "--to"),
        (vec!["data", "seed-demo", "--days", "30"], "--days"),
        (vec!["data", "seed-demo", "--nope"], "unknown option"),
        // The two ruling-12 arrivals: each refuses what it cannot honour, by name.
        (vec!["data", "export", "--out", "s.parquet"], "needs a spec"),
        (vec!["data", "export", "demo:D:1h"], "--out"),
        (vec!["data", "export", "demo:D:1h", "--out", "s", "--days", "7"], "FETCH"),
        (vec!["data", "fetch-starter", "d:S:1h"], "no spec"),
        (vec!["data", "fetch-starter", "--days", "7"], "--days"),
        (vec!["data", "fetch", "b:S:1h", "--days", "1", "--out", "s"], "--out"),
        // The READ half's own refusals, caught before a socket is opened — see below for why the
        // first of these is the one that matters most.
        (vec!["data", "list", "binance:BTCUSDT:1h"], "kind, venue, symbol-or-group"),
        (vec!["data", "list", "--partial-only"], "coverage"),
        (vec!["data", "coverage", "--gaps"], "epoch-ms"),
        (vec!["data", "coverage", "--kind", "trade"], "across kinds"),
        (vec!["data", "fetch", "b:S:1h", "--days", "1", "--addr", "p:1"], "--addr"),
    ] {
        let out = run(scratch.path(), &args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?} must be a usage error: {err}");
        assert!(err.contains(needle), "{args:?} must say {needle:?}: {err}");
    }
}

/// ⚠ **The refusal that is the whole reason this verb has two halves in it.** `--store` names a
/// hist store on THIS machine, which the read verbs cannot open — they ask a datahub about the
/// store THAT process has open. Ignoring the flag would answer confidently about a completely
/// different store, and an operator who just ran `data fetch --store /srv/hist` has every reason
/// to expect `data list --store /srv/hist` to work. So it is refused, on the rung that promises
/// re-running unchanged cannot succeed, with the flag that DOES reach a remote store in the
/// message.
#[test]
fn a_store_flag_on_a_read_verb_is_refused_and_points_at_the_datahub() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for sub in ["list", "coverage"] {
        let out = run(scratch.path(), &["data", sub, "--store", "/srv/hist"]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{sub}: {err}");
        assert!(err.contains("--store"), "{sub} must name the flag: {err}");
        assert!(err.contains("--addr"), "{sub} must name the flag that does reach one: {err}");
        assert_eq!(stdout(&out), "", "{sub} wrote to stdout while refusing");
    }
}

/// A missing engine is a CONNECT-class failure naming what is missing and how to point at one —
/// the same disposition an unreachable datahub gets, because it is the same kind of problem: the
/// command line was right and the thing it needs is not there.
#[test]
fn a_missing_engine_is_the_connect_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let absent = scratch.path().join("no-such-engine");
    let out = run(
        scratch.path(),
        &[
            "data",
            "fetch",
            "binance:BTCUSDT:1m",
            "--days",
            "1",
            "--engine",
            absent.to_str().expect("utf-8 temp path"),
        ],
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(3), "a missing engine is connect-class: {err}");
    assert!(err.contains("backtest"), "the message must name the engine: {err}");
    assert!(err.contains("--engine"), "…and how to point at one: {err}");
}

/// THE plumbing: the verb really does spawn the engine, with the flags its own arguments translate
/// into, and folds the child's exit status rather than inventing one.
///
/// ⚠ A SCRIPT stands in for the engine, which is what keeps this test out of a DataFusion build —
/// and it is unix-only for exactly that reason: a `#!` line is what makes a text file executable,
/// and Windows has no equivalent `Command::new` will run.
#[cfg(unix)]
#[test]
fn the_write_half_reaches_the_engine_as_its_own_subcommand() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    // Echoes its argv and exits 0.
    let engine = scratch.path().join("fake-engine");
    std::fs::write(&engine, "#!/bin/sh\necho \"argv: $*\"\n").expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let engine = engine.to_str().expect("utf-8 temp path");

    let out = run(
        scratch.path(),
        &[
            "data",
            "fetch",
            "binance:BTCUSDT:1h",
            "--days",
            "180",
            "--store",
            "/s",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: data fetch binance:BTCUSDT:1h --days 180 --store /s",
        "the verb's product is the engine's argv"
    );

    let out = run(scratch.path(), &["data", "seed-demo", "--engine", engine]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "argv: data seed-demo");

    // ⚠ **The two ruling-12 arrivals reach the engine as themselves.** They had no home in this
    // verb at all before, so this is what proves the move LANDED rather than merely being written
    // into a help string: `vike-cli data export …` and `vike-cli data fetch-starter` become the
    // engine subcommand of the same name, and the argv reads as the same words on both sides.
    let out = run(scratch.path(), &["data", "fetch-starter", "--store", "/s", "--engine", engine]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "argv: data fetch-starter --store /s");

    let out = run(
        scratch.path(),
        &["data", "export", "demo:D:1h", "--out", "/o.parquet", "--from", "5", "--engine", engine],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: data export demo:D:1h --out /o.parquet --from 5",
        "…and a LONE --from is a well-formed export bound, where on a fetch it is a usage error"
    );
}

/// ⚠ The engine's `2` is NOT re-published as this binary's `2`, and that is the point of this case
/// rather than an accident of it. The engine returns `2` for a bad command line AND for a failed
/// venue fetch, an unopenable store and a failed demo seed — so folding it onto the usage rung
/// would tell a wrapper that a geoblocked `data fetch` "cannot succeed if re-run unchanged", which
/// is false and is the exact retry-vs-fix inversion the ladder exists to remove.
/// `crates/vike-cli/src/cmd/engine.rs`'s `fold_status` carries the argument. What a caller DOES
/// get is the child's own diagnostic, uncaptured, plus the code itself in this binary's line.
#[cfg(unix)]
#[test]
fn an_overloaded_engine_code_lands_on_the_unclassified_rung() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("refusing-engine");
    // 2 is the engine's own usage/pre-flight/runtime code — the build without `venue-fetch`
    // answers with it, and so does a fetch the venue refused. One code, two dispositions, which is
    // why this side may not read a cause into it.
    std::fs::write(&engine, "#!/bin/sh\necho 'no network fetch in this build' >&2\nexit 2\n")
        .expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let out = run(
        scratch.path(),
        &[
            "data",
            "fetch",
            "binance:BTCUSDT:1h",
            "--days",
            "1",
            "--engine",
            engine.to_str().expect("utf-8 temp path"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "an overloaded engine code is the UNCLASSIFIED rung, never the usage one: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("no network fetch"),
        "the child's own diagnostic reaches the user's stderr, uncaptured: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("exited 2"),
        "…and this side names the code it saw rather than asserting a cause for it: {}",
        stderr(&out)
    );
}

/// `--json` makes stdout ONE document and moves the engine's report to stderr.
///
/// The two halves are one property: this crate's rule is that stdout under `--json` is the document
/// and nothing else (`crate::cmd::secrets`'s `list`, `crate::cmd::init`), and the engine writes its
/// human report on the stdout this process would otherwise inherit. If that report were left where
/// it is, every caller would be parsing a stream that is not JSON — so it is moved rather than
/// dropped, and the document carries the same lines verbatim.
#[cfg(unix)]
#[test]
fn json_is_the_whole_of_stdout_and_the_engines_report_moves_to_stderr() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("fake-engine");
    // Two stdout lines shaped like the real engine's fetch report, and one stderr line, so this
    // case can tell "moved" from "merged".
    std::fs::write(
        &engine,
        "#!/bin/sh\necho 'fetching binance/BTCUSDT 1h'\necho '  12 bars returned, 12 rows \
         written'\necho 'a diagnostic' >&2\n",
    )
    .expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let engine = engine.to_str().expect("utf-8 temp path");

    let out = run(
        scratch.path(),
        &[
            "data",
            "fetch",
            "binance:BTCUSDT:1h",
            "--days",
            "180",
            "--store",
            "/s",
            "--json",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    // Stdout parses WHOLE. Not "contains a document" — the engine's lines leaking onto it would
    // still leave a `{` in there, and `serde_json` over the whole stream is what catches that.
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["subcommand"], "fetch");
    assert_eq!(doc["store"], "/s");
    assert_eq!(doc["series"]["venue"], "binance");
    assert_eq!(doc["series"]["symbol"], "BTCUSDT");
    assert_eq!(doc["series"]["interval"], "1h");
    assert_eq!(doc["series"]["spec"], "binance:BTCUSDT:1h");
    assert_eq!(doc["window"]["days"], "180");
    assert_eq!(doc["engine"], engine);
    assert_eq!(
        doc["engine_argv"],
        serde_json::json!([
            "data",
            "fetch",
            "binance:BTCUSDT:1h",
            "--days",
            "180",
            "--store",
            "/s"
        ]),
        "the document carries the argv the engine was actually handed"
    );
    assert_eq!(
        doc["report"],
        serde_json::json!(["fetching binance/BTCUSDT 1h", "  12 bars returned, 12 rows written"]),
        "the engine's own lines travel VERBATIM — the counts are in them, and nothing here parses \
         them into fields it would then get wrong when a word moves"
    );

    // ...and a person still sees the report, on the stream every diagnostic in this crate uses.
    let err = stderr(&out);
    for line in ["fetching binance/BTCUSDT 1h", "12 bars returned", "a diagnostic"] {
        assert!(err.contains(line), "the engine's {line:?} must reach stderr: {err}");
    }
}

/// `seed-demo --json` — the other subcommand, whose request has no series and no window. Both are
/// `null` rather than absent, so a caller can tell "this verb takes none" from "the field is gone".
#[cfg(unix)]
#[test]
fn seed_demo_json_reports_a_run_with_no_series_and_no_window() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("fake-engine");
    std::fs::write(&engine, "#!/bin/sh\necho 'seeded SYNTHETIC demo bars'\n")
        .expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let out = run(
        scratch.path(),
        &["data", "seed-demo", "--json", "--engine", engine.to_str().expect("utf-8 temp path")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["subcommand"], "seed-demo");
    assert!(doc["series"].is_null());
    assert!(doc["window"].is_null());
    assert!(doc["store"].is_null(), "no --store was given, so there is no path this side knows");
    assert_eq!(doc["engine_argv"], serde_json::json!(["data", "seed-demo"]));
    assert_eq!(doc["report"], serde_json::json!(["seeded SYNTHETIC demo bars"]));
}

/// WITHOUT `--json`, the output is what it was before the flag existed — the engine's stdout
/// inherited, nothing captured, nothing added. Stated as its own case because "the new flag changed
/// the old path" is the regression a `--json` addition actually causes, and the existing spawn case
/// would still pass if a document had started appearing beneath the report.
#[cfg(unix)]
#[test]
fn without_json_the_output_is_the_engines_own_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("fake-engine");
    std::fs::write(&engine, "#!/bin/sh\necho \"argv: $*\"\n").expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let out = run(
        scratch.path(),
        &["data", "seed-demo", "--engine", engine.to_str().expect("utf-8 temp path")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "argv: data seed-demo\n", "the human path is unchanged");
}

/// A failing engine under `--json` writes NO document, exits on the same rung the human path exits
/// on, and leaves the child's own diagnostic on stderr.
///
/// ⚠ Both halves are deliberate and each is a sibling's rule. No document, because in this crate a
/// failure is a sentence on stderr plus a rung — `secrets`, `init` and `backtest` all behave that
/// way, and a `{"ok": false}` here would make `data` the one verb a caller has to special-case. The
/// SAME rung, because folding the child's status differently under `--json` would make an output
/// format decide whether a wrapper retries.
#[cfg(unix)]
#[test]
fn a_failing_engine_under_json_writes_no_document_and_keeps_the_rung() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("refusing-engine");
    // A line on stdout BEFORE the failure, so this proves the document is withheld rather than
    // merely that the child printed nothing.
    std::fs::write(
        &engine,
        "#!/bin/sh\necho 'fetching'\necho 'no network fetch in this build' >&2\nexit 2\n",
    )
    .expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let engine = engine.to_str().expect("utf-8 temp path");

    let out = run(
        scratch.path(),
        &["data", "fetch", "binance:BTCUSDT:1h", "--days", "1", "--json", "--engine", engine],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "--json must not change the rung the engine's status folds onto: {}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "", "a failed run must write no document: {}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains("no network fetch"), "the child's own diagnostic must survive: {err}");
    assert!(err.contains("exited 2"), "and this side names the code it saw: {err}");
    assert!(err.contains("fetching"), "and the report it did print reaches stderr: {err}");
}

/// The flag is refused where it is not supported, on the SAME rung its siblings refuse on.
///
/// `trade` declines `--json` on purpose and says so in its own usage text (a `--json` REPL is what
/// `mcp` already is); `data` itself refuses one before any subcommand is chosen, because there is
/// no run for a document to describe. Both are the USAGE rung — the one that promises "nothing was
/// attempted, and re-running unchanged cannot succeed".
#[test]
fn json_is_refused_where_it_is_not_supported() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data", "--json"], "subcommand"),
        (vec!["data", "frobnicate", "--json"], "unknown `data` subcommand"),
        (vec!["trade", "--json"], "--json"),
    ] {
        let out = run(scratch.path(), &args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?} must be the usage rung: {err}");
        assert!(err.contains(needle), "{args:?} must say {needle:?}: {err}");
        assert_eq!(stdout(&out), "", "{args:?} wrote to stdout while refusing: {}", stdout(&out));
    }
}

// ─── the read half, against a REAL loopback datahub ─────────────────────────────────────────────

/// Epoch-ms per UTC day — the seeded fixture's step, so its three bars land on three distinct days
/// and the `DAYS` column has something to count.
const DAY_MS: i64 = 86_400_000;

/// A bar for the seeded store. The prices are irrelevant; the TIMESTAMP is not — the coverage this
/// verb renders is folded from it.
fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Bind an ephemeral loopback listener, seed an in-memory store with TWO series of DIFFERENT kinds
/// — a `bar` series (which sub-partitions by interval) and a `properties` series (which has none) —
/// and serve it on a detached thread.
///
/// Two kinds is the point rather than convenience: a listing that assumed one shape would render
/// the other wrongly, and a single-series fixture could not tell a real `interval` column from a
/// hardcoded one.
///
/// ⚠ **`properties` rather than `trade`, and that is a measurement rather than a taste.**
/// `MemHistStore::append_trades` is a NO-OP returning `Ok(0)` — the double holds no tick maps at
/// all — so a trade-seeded fixture reports a successful seed, enumerates ONE series, and leaves
/// every assertion below silently weaker than it reads. `MemHistStore::catalog` is the authority
/// for which kinds this double can actually enumerate; `properties` is one of them and, like every
/// tick-shaped kind, carries no interval, which is the property this fixture needs.
fn spawn_seeded_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = MemHistStore::new();
    store
        .append_bars("binance", "BTCUSDT", "1h", &[bar(0), bar(DAY_MS), bar(2 * DAY_MS)], None)
        .expect("seed bars");
    store
        .append_symbol_properties("okx", "BTC-USDT", &[(DAY_MS, SymbolProperties::default())], None)
        .expect("seed properties");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// A datahub that is not there is the CONNECT rung — the one a wrapper backs off and retries on,
/// and the same disposition (and the same sentence) `vike-cli backtest` gives for the same socket.
///
/// ⚠ The address is a port this test BOUND and then released, rather than a low port assumed to be
/// closed: a privileged port can be occupied on a shared runner, and a case that "proved" a refusal
/// against somebody else's listener would be proving nothing.
#[test]
fn an_unreachable_datahub_is_the_connect_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let bound = listener.local_addr().expect("resolve assigned port");
        bound.to_string()
        // ...and the listener is dropped here, so nothing is listening on that port.
    };
    for sub in ["list", "coverage"] {
        let out = run(scratch.path(), &["data", sub, "--addr", &addr]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(3), "{sub} must be connect-class: {err}");
        assert!(err.contains("cannot connect to datahub"), "{sub}: {err}");
        assert!(err.contains(&addr), "{sub} must name the address: {err}");
        assert_eq!(stdout(&out), "", "{sub} wrote a document for a run that never happened");
    }
}

/// THE plumbing for `list`: the verb reaches a real datahub's `inventory()` and carries every
/// dimension of what came back.
///
/// ⚠ The document carries the RAW `symbol` and `group` beside the derived `name`, which is the
/// property a `VENUE:SYMBOL:INTERVAL` rendering cannot have. Both of this fixture's series are
/// per-symbol (the in-memory double holds no grouped series), so the GROUPED half of that contract
/// is pinned by the unit tests beside the module; what this case proves is that the fields survive
/// the wire at all, and that a tick series' `interval` arrives as `null` rather than as an empty
/// string somebody invented on the way.
#[test]
fn list_reaches_a_real_datahub_and_carries_every_dimension() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "list", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["subcommand"], "list");
    assert_eq!(doc["addr"], addr);
    assert_eq!(doc["series_reported"], 2);
    assert_eq!(doc["count"], 2);
    assert_eq!(doc["gaps_requested"], false);

    // The store's enumeration is sorted by id, so `bar` precedes `properties`.
    let bars = &doc["series"][0];
    assert_eq!(bars["kind"], "bar");
    assert_eq!(bars["venue"], "binance");
    assert_eq!(bars["name"], "BTCUSDT");
    assert_eq!(bars["symbol"], "BTCUSDT");
    assert!(bars["group"].is_null());
    assert_eq!(bars["grouped"], false);
    assert_eq!(bars["interval"], "1h", "a bar series sub-partitions by its step");
    assert_eq!(bars["coverage"]["rows"], 3);
    assert_eq!(bars["coverage"]["first_ts"], 0);
    assert_eq!(bars["coverage"]["last_ts"], 2 * DAY_MS);
    assert!(bars["gaps"].is_null(), "no --gaps was asked for, so the field is null not empty");

    let ticks = &doc["series"][1];
    assert_eq!(ticks["kind"], "properties");
    assert_eq!(ticks["venue"], "okx");
    assert_eq!(ticks["name"], "BTC-USDT");
    assert!(ticks["interval"].is_null(), "a tick-shaped series genuinely has no interval");
    assert_eq!(ticks["coverage"]["rows"], 1);
}

/// The filter is applied CLIENT-SIDE to what the server sent, and the document carries BOTH counts
/// — which is what lets a caller tell an empty store from an over-narrow filter in one call.
#[test]
fn the_list_filter_narrows_the_rows_and_both_counts_are_reported() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "list", "--addr", &addr, "--venue", "OKX", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["count"], 1, "case-insensitive substring on the venue");
    assert_eq!(doc["series_reported"], 2, "…beside what the server actually reported");
    assert_eq!(doc["series"][0]["venue"], "okx");
    assert_eq!(doc["filter"]["venue"], "OKX", "the filter is echoed as typed");

    // A filter that matches nothing is a successful run with an honest empty, never a failure.
    let out = run(scratch.path(), &["data", "list", "--addr", &addr, "--name", "NOSUCH"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no series match the filter (2 reported)"), "{}", stdout(&out));
}

/// `--gaps` reaches `series_gaps` per matched series, and an EMPTY answer is rendered as an
/// answered "no gaps" rather than as silence — the distinction an operator typed the flag for.
#[test]
fn gaps_are_fetched_per_series_and_an_empty_answer_is_said_out_loud() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "list", "--addr", &addr, "--gaps", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["gaps_requested"], true);
    for i in 0..2 {
        let gaps = doc["series"][i]["gaps"].as_array().expect("--gaps was asked for");
        assert!(gaps.is_empty(), "series {i} reported holes it does not have: {gaps:?}");
        assert!(doc["series"][i]["gaps_error"].is_null(), "series {i}");
    }

    let out = run(scratch.path(), &["data", "list", "--addr", &addr, "--gaps"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).matches("no gaps").count(),
        2,
        "each matched series gets an explicit verdict: {}",
        stdout(&out)
    );
}

/// The human table's identity columns, against a real answer: `kind` and `SCOPE` are their own
/// cells and nothing is joined into a colon-string.
#[test]
fn the_human_listing_renders_columns_and_never_a_colon_string() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "list", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for token in ["KIND", "VENUE", "SCOPE", "NAME", "INTERVAL", "ROWS", "DAYS", "FIRST", "LAST"] {
        assert!(text.contains(token), "the header must carry {token}: {text}");
    }
    assert!(text.contains("bar") && text.contains("properties"), "both kinds are rows: {text}");
    assert!(text.contains("symbol"), "the scope cell says which alternative the name is: {text}");
    assert!(!text.contains("binance:BTCUSDT"), "no colon-string identity: {text}");
    assert!(text.trim_end().ends_with("2 series · 4 rows"), "the summary line: {text}");
}

/// `coverage` reaches the cross-kind verb and renders an EMPTY report honestly.
///
/// ⚠ What is pinned here is the RENDERING, not the report. The in-memory double inherits
/// `HistStore::coverage_report`'s default — an empty `Ok` — so this proves the request reaches the
/// server and comes back, and that a zero-row answer reads as a sentence rather than as a silently
/// blank table. A report with rows IN it is folded by the unit tests beside the module, which is
/// where it can be done without a `DataFusionHist` this crate may not link.
#[test]
fn coverage_reaches_the_cross_kind_verb_and_renders_an_empty_report_honestly() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "coverage", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["subcommand"], "coverage");
    assert_eq!(doc["addr"], addr);
    assert_eq!(doc["instruments_reported"], 0);
    assert_eq!(doc["count"], 0);
    assert!(doc["instruments"].as_array().expect("an array, even when empty").is_empty());

    let out = run(scratch.path(), &["data", "coverage", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("the datahub reported no instruments at all"),
        "an empty report is a sentence, not a blank table: {}",
        stdout(&out)
    );
}

// ─── `rm`: the destructive verb ─────────────────────────────────────────────────────────────────

/// The command-line refusals `rm` owns, every one caught HERE — before a process is spawned or a
/// socket opened, so the diagnostic comes from the binary the operator typed and lands on the rung
/// that promises re-running unchanged cannot succeed.
///
/// ⚠ The last row is the one the whole verb turns on: **no `--yes` and no terminal is a REFUSAL,
/// never a read.** A test binary's stdin is not a terminal, which is exactly the shape a CI step,
/// a systemd `ExecStart` and `yes | vike-cli data rm …` all have.
#[test]
fn rm_refuses_a_bad_command_line_before_it_touches_anything() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data", "rm"], "--kind"),
        (vec!["data", "rm", "--kind", "bar"], "--venue"),
        (
            vec!["data", "rm", "--kind", "bar", "--venue", "b", "--symbol", "S", "--group", "G"],
            "ALTERNATIVES",
        ),
        (
            vec![
                "data",
                "rm",
                "--kind",
                "book",
                "--venue",
                "p",
                "--group",
                "G",
                "--interval",
                "1h",
            ],
            "--interval does not apply",
        ),
        (vec!["data", "rm", "--kind", "bar", "--venue", "b", "--symbol", "BTC*"], "glob character"),
        (vec!["data", "rm", "--kind", "bar", "--venue", "b", "--symbol", " "], "EMPTY value"),
        // ⚠ **The blank PREFIX, in both spellings, on both routes.** There was no row for it —
        // the loop above happened to catch it, with a sentence about the store's grouped-series
        // `symbol=` sentinel that is false of a provenance assertion and names the opposite
        // remedy. The needle is the CONSEQUENCE, so the wrong refusal cannot satisfy it.
        (
            vec!["data", "rm", "--kind", "bar", "--venue", "b", "--produced-by", ""],
            "matches every key",
        ),
        (
            vec!["data", "rm", "--kind", "bar", "--venue", "b", "--produced-by="],
            "matches every key",
        ),
        (
            vec![
                "data",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "b",
                "--produced-by",
                "",
                "--addr",
                "127.0.0.1:1",
            ],
            "matches every key",
        ),
        // ⚠ A PRODUCER PATH under `--addr`. The SERVER half landed on 2026-09-11, so this is a
        // COMPATIBILITY guard now rather than a stand-in: this protocol carries no capability
        // string for "this server resolves producer paths", so a datahub that has not been
        // redeployed still asserts one as a literal prefix, matches no key, and reports the store
        // as foreign.
        (
            vec![
                "data",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "b",
                "--produced-by",
                "crates/vike-data/src/demo.rs",
                "--addr",
                "127.0.0.1:1",
            ],
            "PRODUCER PATH",
        ),
        (vec!["data", "rm", "--kind", "bar", "--venue", "b", "--name", "x"], "LISTING"),
        (vec!["data", "rm", "--kind", "bar", "--venue", "b", "--days", "3"], "FETCH window"),
        (vec!["data", "rm", "--kind", "bar", "--venue", "b", "b:S:1h"], "takes no"),
        (
            vec!["data", "rm", "--kind", "bar", "--venue", "b", "--addr", "h:1", "--store", "/s"],
            "two DIFFERENT stores",
        ),
        // ⚠ THE ONE. Fully named, so no `--produced-by` is needed, and the command line is
        // otherwise perfect — it is refused because nobody can confirm it.
        (
            vec![
                "data",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "b",
                "--symbol",
                "S",
                "--interval",
                "1h",
            ],
            "not a terminal",
        ),
    ] {
        let out = run(scratch.path(), &args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?} must be a usage error: {err}");
        assert!(err.contains(needle), "{args:?} must say {needle:?}: {err}");
        assert_eq!(stdout(&out), "", "{args:?} wrote to stdout while refusing");
    }
}

/// `rm`'s own flags are refused BY NAME on every other subcommand, with the verb that takes them in
/// the message — the rule this module applies to every half-crossing flag.
#[test]
fn rms_flags_are_refused_on_the_other_subcommands() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for sub in ["fetch", "fetch-starter", "seed-demo", "export", "list", "coverage"] {
        let out = run(scratch.path(), &["data", sub, "--produced-by", "panel_bars:"]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{sub}: {err}");
        assert!(err.contains("--produced-by"), "{sub} must name the flag: {err}");
        assert!(err.contains("`rm`"), "{sub} must name the verb that takes it: {err}");
    }
}

/// THE plumbing: `rm` really does spawn the engine, with the flags its own arguments translate
/// into, and an omitted dimension reaches the child as an ABSENT flag rather than an empty one.
///
/// ⚠ A SCRIPT stands in for the engine, unix-only, for the reason
/// `fetch_and_seed_demo_reach_the_engine_as_its_own_flags` gives: a `#!` line is what makes a text
/// file executable, and it keeps this file out of a DataFusion build.
#[cfg(unix)]
#[test]
fn rm_reaches_the_engine_as_its_own_flags() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("fake-engine");
    std::fs::write(&engine, "#!/bin/sh\necho \"argv: $*\"\n").expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let engine = engine.to_str().expect("utf-8 temp path");

    // A fully-named series, confirmed with --yes: one spawn, and the argv is the product.
    let out = run(
        scratch.path(),
        &[
            "data",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "hyperliquid",
            "--symbol",
            "BTC",
            "--interval",
            "1h",
            "--yes",
            "--store",
            "/s",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: data rm --kind bar --venue hyperliquid --symbol BTC --interval 1h --yes --store /s",
        "the verb's product is the engine's argv"
    );

    // A sweep with --produced-by and --dry-run: the wildcarded dimensions are simply ABSENT from
    // the argv, which is what makes an omitted dimension the only wildcard there is.
    let out = run(
        scratch.path(),
        &[
            "data",
            "rm",
            "--kind",
            "cohort",
            "--venue",
            "hyperliquid",
            "--produced-by",
            "cohort:",
            "--dry-run",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: data rm --kind cohort --venue hyperliquid --produced-by cohort: --dry-run",
        "an omitted dimension reaches the engine as an ABSENT flag, never as an empty one"
    );
}

/// ⚠ **`--json` is FORWARDED here**, unlike on `fetch` — and the document nests the ENGINE's own,
/// because that is where the resolved store root and the rung that chose it live. This side cannot
/// know either: the engine resolves them in another process.
#[cfg(unix)]
#[test]
fn rm_json_forwards_the_flag_and_nests_the_engines_document() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("fake-engine");
    let script = concat!(
        "#!/bin/sh\n",
        "echo '{\"store_root\":\"/srv/hist\",\"store_rung\":\"explicit\",\"matched\":0}'\n"
    );
    std::fs::write(&engine, script).expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let engine = engine.to_str().expect("utf-8 temp path");

    let out = run(
        scratch.path(),
        &[
            "data",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "hyperliquid",
            "--symbol",
            "BTC",
            "--interval",
            "1h",
            "--yes",
            "--json",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout under --json is the document, whole");
    assert_eq!(doc["route"], "engine");
    assert_eq!(doc["selector"]["kind"], "bar");
    assert_eq!(doc["selector"]["group"], serde_json::Value::Null, "a wildcard is an explicit null");
    assert!(
        doc["engine_argv"].as_array().expect("argv").iter().any(|v| v == "--json"),
        "--json must be FORWARDED: {doc}"
    );
    assert_eq!(
        doc["engine_report"]["store_root"], "/srv/hist",
        "the engine's document is NESTED, not re-parsed out of prose: {doc}"
    );
    assert_eq!(
        doc["engine_report"]["store_rung"], "explicit",
        "the RUNG is the fact that cannot survive a prose round trip: {doc}"
    );
    assert_eq!(
        doc["store"],
        serde_json::Value::Null,
        "no --store was given, so null, never a guess"
    );
}

/// The REMOTE route reaches a datahub, and a KEY-LESS one refuses the verb it does not advertise —
/// with a message naming the route that does work. This is the client-side half of the posture the
/// server enforces: the advertisement is what stops a well-behaved client sending at all.
#[test]
fn rm_over_a_keyless_datahub_is_refused_with_the_local_route_named() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub();
    let out = run(
        scratch.path(),
        &[
            "data",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--interval",
            "1h",
            "--yes",
            "--addr",
            &addr.to_string(),
        ],
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "a server that ANSWERED is the run-failure rung: {err}");
    assert!(err.contains("delete_series"), "the refusal names the missing capability: {err}");
    assert!(err.contains("--store"), "…and the route that works today: {err}");
    assert_eq!(stdout(&out), "", "nothing was printed as though it had run");
}
