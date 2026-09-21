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

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_model::{AssetClass, Bar, SymbolProperties};

/// The planted-engine plant and the `ETXTBSY` retry that survives spawning one. See that module's
/// doc for the race and for why matching the errno — and only the errno — is what keeps the retry
/// from hiding a genuinely missing engine, which this file has a case about.
mod common;

/// The ONE way this file spawns `vike-cli` (unlike `backtest_cli.rs`, which keeps a dozen direct
/// spawns for the ambient environment they need), so every case — the eight that plant a stand-in
/// engine and the rest that do not — inherits the `ETXTBSY` retry without having to know it exists.
/// `planted_binary_retry.rs`'s `a_case_that_plants_an_engine_may_not_spawn_the_cli_itself` holds
/// that property for both files.
///
/// ⚠ The retry is NOT belt-and-braces here. Each planted engine is exec'd by the CHILD, so a lost
/// coin toss arrives as a `vike-cli` that died before producing anything and the case fails on its
/// own assertion about missing output. `common`'s module doc carries the mechanism; the predicate
/// is the errno alone, and a spawn failure that is not that errno still panics on the first
/// attempt exactly as this function did before.
fn run(settings_dir: &Path, args: &[&str]) -> Output {
    common::output_retrying_etxtbsy(&format!("run vike-cli {args:?}"), || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
        c.args(args)
            .env("VIKE_SETTINGS_DIR", settings_dir)
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL");
        c
    })
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
///
/// ⚠ **This roster is HAND-WRITTEN and the module-side one is DERIVED, so only this copy can go
/// short.** The unit test beside `crates/vike-cli/src/cmd/data.rs`'s `SUBCOMMANDS` builds its
/// expectation from that const and therefore cannot go short; an integration test
/// cannot see a private const at all. The failure that shape produces is SUBTRACT-ONLY and
/// silent — a sub-verb missing here simply is not checked, and the file stays green — which is the
/// class `crates/vike-ops/tests/path_key_gate.rs` is named for. So: **adding a `data` sub-verb
/// means adding it HERE**, and the bare-`data` refusal below is the derived cross-check that will
/// name it whether or not you remember.
#[test]
fn help_names_every_subcommand_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ The verb NAMES, as they are after the group split — `list` is `ls` and `tape-health` is
    // `health` now. The help text advertises them under `data hist`, so a bare name here would
    // pass on a substring of the group line and prove nothing.
    for sub in [
        "fetch",
        "fetch-starter",
        "seed-demo",
        "export",
        "ls",
        "coverage",
        "health",
        "universe",
        "rm",
        "repair",
    ] {
        assert!(text.contains(sub), "`data --help` must list `{sub}`: {text}");
    }
    // ⚠ **The DERIVED cross-check, and it is what stops this file being subtract-only.** The
    // bare-`data` refusal renders `SUBCOMMANDS` itself — `a subcommand is required (a | b | …)` —
    // so reading the roster back OUT of that sentence gives this process the private const it
    // cannot name. A sub-verb added to the module and forgotten in the literal above is then a RED
    // test here rather than a silently narrower one. Nothing is parsed twice: the literal stays
    // because it is what pins the NAMES an operator types, and this half pins the SET.
    let bare = run(scratch.path(), &["data"]);
    let refusal = stderr(&bare);
    let roster = refusal
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(inner, _)| inner.split('|').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_else(|| panic!("the bare-`data` refusal must render its roster: {refusal}"));
    assert!(roster.len() >= 9, "the roster looks truncated: {roster:?}");
    for sub in &roster {
        assert!(
            text.contains(sub),
            "`data --help` must list the DERIVED subcommand `{sub}`: {text}"
        );
    }
}

/// A command line the user can fix exits on the USAGE rung, and every one of these is caught HERE —
/// before a process is spawned, so the diagnostic comes from the binary they typed.
#[test]
fn a_bad_command_line_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data"], "group"),
        (vec!["data", "frobnicate"], "unknown `data` group"),
        (vec!["data", "hist", "fetch"], "VENUE:SYMBOL:INTERVAL"),
        (vec!["data", "hist", "fetch", "binance:BTCUSDT"], "VENUE:SYMBOL:INTERVAL"),
        (vec!["data", "hist", "fetch", "binance:BTCUSDT:1h"], "--days"),
        (vec!["data", "hist", "fetch", "binance:BTCUSDT:1h", "--days", "x"], "--days"),
        (vec!["data", "hist", "fetch", "binance:BTCUSDT:1h", "--from", "0"], "--to"),
        (vec!["data", "hist", "seed-demo", "--days", "30"], "--days"),
        (vec!["data", "hist", "seed-demo", "--nope"], "unknown option"),
        // The two ruling-12 arrivals: each refuses what it cannot honour, by name.
        (vec!["data", "hist", "export", "--out", "s.parquet"], "needs a spec"),
        (vec!["data", "hist", "export", "demo:D:1h"], "--out"),
        (vec!["data", "hist", "export", "demo:D:1h", "--out", "s", "--days", "7"], "FETCH"),
        (vec!["data", "hist", "fetch-starter", "d:S:1h"], "no spec"),
        (vec!["data", "hist", "fetch-starter", "--days", "7"], "--days"),
        (vec!["data", "hist", "fetch", "b:S:1h", "--days", "1", "--out", "s"], "--out"),
        // The READ half's own refusals, caught before a socket is opened — see below for why the
        // first of these is the one that matters most.
        (vec!["data", "hist", "ls", "binance:BTCUSDT:1h"], "kind, venue, symbol-or-group"),
        (vec!["data", "hist", "ls", "--partial-only"], "coverage"),
        (vec!["data", "hist", "coverage", "--gaps"], "epoch-ms"),
        (vec!["data", "hist", "coverage", "--kind", "trade"], "across kinds"),
        // ⚠ `--addr` on `fetch` used to be refused HERE and is now the route itself — the verb asks
        // a datahub and nothing else. What replaces it is the mirror refusal: the two flags that
        // name a LOCAL store and the engine that opens it, which `fetch` no longer has.
        (vec!["data", "hist", "fetch", "b:S:1h", "--days", "1", "--store", "/tmp/s"], "--store"),
        (vec!["data", "hist", "fetch", "b:S:1h", "--days", "1", "--engine", "/tmp/e"], "--engine"),
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
    for sub in ["ls", "coverage"] {
        let out = run(scratch.path(), &["data", "hist", sub, "--store", "/srv/hist"]);
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
        &["data", "hist", "seed-demo", "--engine", absent.to_str().expect("utf-8 temp path")],
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
    let scratch = tempfile::tempdir().expect("tempdir");
    // Echoes its argv and exits 0.
    let planted =
        common::plant_engine(scratch.path(), "fake-engine", "#!/bin/sh\necho \"argv: $*\"\n");
    let engine = planted.arg();

    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "export",
            "binance:BTCUSDT:1h",
            "--out",
            "s.parquet",
            "--store",
            "/s",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: data export binance:BTCUSDT:1h --out s.parquet --store /s",
        "the verb's product is the engine's argv"
    );

    let out = run(scratch.path(), &["data", "hist", "seed-demo", "--engine", engine]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "argv: data seed-demo");

    // ⚠ **The two ruling-12 arrivals reach the engine as themselves.** They had no home in this
    // verb at all before, so this is what proves the move LANDED rather than merely being written
    // into a help string: `vike-cli data export …` and `vike-cli data fetch-starter` become the
    // engine subcommand of the same name, and the argv reads as the same words on both sides.
    let out = run(
        scratch.path(),
        &["data", "hist", "fetch-starter", "--store", "/s", "--engine", engine],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "argv: data fetch-starter --store /s");

    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "export",
            "demo:D:1h",
            "--out",
            "/o.parquet",
            "--from",
            "5",
            "--engine",
            engine,
        ],
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
    let scratch = tempfile::tempdir().expect("tempdir");
    // 2 is the engine's own usage/pre-flight/runtime code — the build without `venue-fetch`
    // answers with it, and so does a fetch the venue refused. One code, two dispositions, which is
    // why this side may not read a cause into it.
    let planted = common::plant_engine(
        scratch.path(),
        "refusing-engine",
        "#!/bin/sh\necho 'no network fetch in this build' >&2\nexit 2\n",
    );

    let out = run(scratch.path(), &["data", "hist", "seed-demo", "--engine", planted.arg()]);
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
    let scratch = tempfile::tempdir().expect("tempdir");
    // Two stdout lines shaped like the real engine's fetch report, and one stderr line, so this
    // case can tell "moved" from "merged".
    let planted = common::plant_engine(
        scratch.path(),
        "fake-engine",
        "#!/bin/sh\necho 'fetching binance/BTCUSDT 1h'\necho '  12 bars returned, 12 rows \
         written'\necho 'a diagnostic' >&2\n",
    );
    let engine = planted.arg();

    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "export",
            "binance:BTCUSDT:1h",
            "--out",
            "s.parquet",
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

    assert_eq!(doc["subcommand"], "export");
    assert_eq!(doc["store"], "/s");
    assert_eq!(doc["series"]["venue"], "binance");
    assert_eq!(doc["series"]["symbol"], "BTCUSDT");
    assert_eq!(doc["series"]["interval"], "1h");
    assert_eq!(doc["series"]["spec"], "binance:BTCUSDT:1h");
    // ⚠ No `window` assertion: this case was a `fetch` until that verb left the engine route, and
    // `export` carries its bounds as `export_range` rather than a `Window`. Both are unset here,
    // and `report_json` renders an absent window as NULL — the property the `seed-demo` case in the
    // module-side test pins by name.
    assert!(
        doc["window"]["from"].is_null() && doc["window"]["to"].is_null(),
        "export with no bounds renders its range with BOTH ends null — present-and-empty rather \
         than absent, so a machine can tell 'no bounds' from 'the field is gone': {doc}"
    );
    assert_eq!(doc["engine"], engine);
    assert_eq!(
        doc["engine_argv"],
        serde_json::json!([
            "data",
            "export",
            "binance:BTCUSDT:1h",
            "--out",
            "s.parquet",
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
    let scratch = tempfile::tempdir().expect("tempdir");
    let planted = common::plant_engine(
        scratch.path(),
        "fake-engine",
        "#!/bin/sh\necho 'seeded SYNTHETIC demo bars'\n",
    );

    let out =
        run(scratch.path(), &["data", "hist", "seed-demo", "--json", "--engine", planted.arg()]);
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
    let scratch = tempfile::tempdir().expect("tempdir");
    let planted =
        common::plant_engine(scratch.path(), "fake-engine", "#!/bin/sh\necho \"argv: $*\"\n");

    let out = run(scratch.path(), &["data", "hist", "seed-demo", "--engine", planted.arg()]);
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
    let scratch = tempfile::tempdir().expect("tempdir");
    // A line on stdout BEFORE the failure, so this proves the document is withheld rather than
    // merely that the child printed nothing.
    let planted = common::plant_engine(
        scratch.path(),
        "refusing-engine",
        "#!/bin/sh\necho 'fetching'\necho 'no network fetch in this build' >&2\nexit 2\n",
    );
    let engine = planted.arg();

    let out = run(scratch.path(), &["data", "hist", "seed-demo", "--json", "--engine", engine]);
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
        (vec!["data", "--json"], "group"),
        (vec!["data", "frobnicate", "--json"], "unknown `data` group"),
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

/// A datahub over a store seeded to hold ALL THREE class outcomes at once — the fixture for
/// `--class`, and a SECOND one rather than a widening of [`spawn_seeded_datahub`] because two of
/// this file's cases pin that store's series COUNT and a third instrument would break them for a
/// reason having nothing to do with what they test.
///
/// * `binance/BTCUSDT` — bars, and NO properties row anywhere. The `no-properties` verdict: nothing
///   has ever recorded an instrument grid for it.
/// * `bybit/BTCUSDT` — one properties row whose `asset_class` is `None`. The `unclassified`
///   verdict, and the signal this whole flag exists for: a producer recorded a grid and named no
///   class. `SymbolProperties::default()` genuinely leaves it `None`, so this is the real shape a
///   not-yet-wired venue writes rather than a mock of one.
/// * `okx/BTC-USDT-SWAP` — TWO properties rows, the earlier naming no class and the later naming
///   [`AssetClass::CryptoPerp`]. Two rather than one, so the case also proves the as-of rule: the
///   probe asks `properties_as_of` at `i64::MAX` and must come back with the LATER answer. A
///   single-row fixture would pass identically whether the code took the first or the last.
fn spawn_class_probe_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = MemHistStore::new();
    store.append_bars("binance", "BTCUSDT", "1h", &[bar(0), bar(DAY_MS)], None).expect("seed bars");
    store
        .append_symbol_properties(
            "bybit",
            "BTCUSDT",
            &[(DAY_MS, SymbolProperties::default())],
            None,
        )
        .expect("seed a class-less grid");
    store
        .append_symbol_properties(
            "okx",
            "BTC-USDT-SWAP",
            &[
                (DAY_MS, SymbolProperties::default()),
                (
                    2 * DAY_MS,
                    SymbolProperties {
                        asset_class: Some(AssetClass::CryptoPerp),
                        ..SymbolProperties::default()
                    },
                ),
            ],
            None,
        )
        .expect("seed a classified grid");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// **The READ `vike_model::SymbolProperties::asset_class` did not have**, end to end: a venue
/// producer's recorded class travels store → `properties_as_of` → wire → the operator's table.
///
/// ⚠ **What this case is FOR.** Until `--class`, `git grep` found no production code in the
/// workspace that read that field back — `scan_symbol_properties` was called from the bridges'
/// `filters_rec.rs` TEST modules and nowhere else. A stored field nothing reads is a field that
/// goes wrong in silence: a venue writing the wrong class, or writing none, is invisible. So the
/// assertion that matters most here is not that `CryptoPerp` arrives — it is that the two ways of
/// having NO class arrive as two different words.
#[test]
fn the_recorded_asset_class_reaches_the_operator_and_its_absences_are_two_words() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_class_probe_datahub().to_string();

    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--class", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["class_requested"], true);
    let series = doc["series"].as_array().expect("an array of rows");
    assert_eq!(series.len(), 3, "the fixture's three instruments: {series:?}");

    // Indexed by venue rather than by position: the enumeration's sort order is the store's
    // business, and a case that encoded it would fail on a store that sorted differently.
    let by_venue = |venue: &str| -> &serde_json::Value {
        series.iter().find(|s| s["venue"] == venue).unwrap_or_else(|| panic!("{venue} is a row"))
    };

    let okx = by_venue("okx");
    assert_eq!(okx["asset_class_status"], "classified");
    assert_eq!(
        okx["asset_class"], "CryptoPerp",
        "the LATER of the two recorded rows — the probe is as-of now, not as-of the first row"
    );

    let bybit = by_venue("bybit");
    assert_eq!(bybit["asset_class_status"], "unclassified", "a grid was recorded, naming no class");
    assert!(bybit["asset_class"].is_null(), "…so there is no word to carry: {bybit}");

    let binance = by_venue("binance");
    assert_eq!(binance["asset_class_status"], "unrecorded", "no properties row at all");
    assert!(binance["asset_class"].is_null());

    // Nothing failed, so no row carries a reason.
    for row in series {
        assert!(row["asset_class_error"].is_null(), "{row}");
    }

    // ...and the human table, where the operator actually reads it.
    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--class"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("CLASS"), "the column has a header: {text}");
    assert!(text.contains("CryptoPerp"), "{text}");
    assert!(text.contains("unclassified"), "the producer-not-wired signal: {text}");
    assert!(text.contains("no-properties"), "…and the never-recorded one, distinctly: {text}");

    // WITHOUT the flag the column is not there — the probe is opt-in, and a listing that paid for
    // it unasked would cost one round trip per instrument on every `data list`.
    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("CLASS"), "{}", stdout(&out));
    assert!(!stdout(&out).contains("CryptoPerp"), "{}", stdout(&out));
}

/// `--class` is refused BY NAME on the three read siblings, each with its own reason — never
/// silently ignored, which is this verb's standing rule for a flag that does not apply.
///
/// ⚠ No datahub is spawned and none is needed: the refusal is a PARSE result, so it lands before a
/// socket is opened. That is the property worth having — a flag that could not be honoured must not
/// cost a connection to discover.
#[test]
fn the_class_flag_is_refused_on_the_sibling_read_verbs() {
    let scratch = tempfile::tempdir().expect("tempdir");

    for sub in ["coverage", "health", "universe"] {
        let out = run(scratch.path(), &["data", "hist", sub, "--class"]);
        assert_eq!(out.status.code(), Some(2), "a bad command line is the usage rung: {sub}");
        let err = stderr(&out);
        assert!(err.contains("--class"), "{sub} must name the flag it refused: {err}");
        assert!(err.contains("data hist ls --class"), "…and where the class is: {err}");
    }
}

/// How many times [`an_unreachable_datahub_is_the_connect_rung`] re-rolls onto a fresh port when
/// its premise — that nothing is listening on the address it chose — is MEASURED to be false.
/// Bounded, and exhausting it PANICS: a case that skipped itself when the port was stolen would be
/// worse than the flake it replaces.
const PREMISE_ATTEMPTS: usize = 8;

/// How long the premise probe waits for a loopback connect to answer. A closed loopback port
/// refuses immediately and an open one accepts immediately, so this bounds only the pathological
/// case; it is not a tuning knob any assertion depends on.
const PREMISE_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Is anything listening on `addr` RIGHT NOW? The strongest premise check available here — the
/// same syscall, to the same address, that the child process is about to make.
fn is_listening(addr: &SocketAddr) -> bool {
    TcpStream::connect_timeout(addr, PREMISE_PROBE_TIMEOUT).is_ok()
}

/// A datahub that is not there is the CONNECT rung — the one a wrapper backs off and retries on,
/// and the same disposition (and the same sentence) `vike-cli backtest` gives for the same socket.
///
/// ⚠ The address is a port this test BOUND and then released, rather than a low port assumed to be
/// closed: a privileged port can be occupied on a shared runner, and a case that "proved" a refusal
/// against somebody else's listener would be proving nothing. That argument stands, and is why the
/// cure below is not "go back to a low port" — it is the same objection, answered for both sides.
///
/// ⚠ **A released port is not a port that STAYS free, and that is how this case reddened a green
/// `main`** (run 34941165673: `left: Some(0)` — the CLI exited SUCCESSFULLY, having reached
/// somebody's real listener on the port this test had just let go of). Every process on a shared
/// runner draws from one ephemeral range, this file's own [`spawn_seeded_datahub`] included. So
/// the premise is measured rather than assumed: each child run is BRACKETED by a connect probe of
/// its own, and the two outcomes are never conflated —
///
/// * either probe answers → somebody holds the port, the premise is false, and NOTHING is asserted
///   about the CLI; the attempt re-rolls onto a fresh port and the reason is kept for the report.
/// * both probes refuse → the address was closed on both sides of the run, so the exit code, the
///   sentence, the echoed address and the empty stdout are the CLI's own answer, asserted exactly
///   as before.
///
/// **This still fails for its stated reason.** A CLI that answers with any rung but connect-class
/// reddens on the FIRST attempt: the port it is handed stays closed, both probes refuse, and the
/// assertion runs — proven by mutating `crates/vike-cli/src/cmd/data.rs`'s `connect` to a
/// different `CliError` constructor and watching this case fail on `Some(2)`. Re-rolling is
/// bounded by [`PREMISE_ATTEMPTS`] and exhausting it panics with every reason listed, so a runner
/// that somehow stole every port is a loud failure rather than a skip.
///
/// The accepted residual: a thief that both arrives and departs INSIDE one run's window is
/// invisible to both probes, and would be reported as a CLI defect. That direction is deliberate —
/// a premise check generous enough to excuse it could excuse a real defect too.
///
/// The probe is the same primitive `crates/vike-agent-eval/src/node.rs`'s `pick_port` already uses
/// against the OTHER half of this class — a harness that must BIND the port it chose, which it
/// answers by drawing below the kernel's ephemeral floor so the range is never handed out under
/// it. That trick does not transfer: a case that needs a REFUSAL wants a port nobody has a reason
/// to serve on, which the ephemeral range gives and a fixed low band does not.
#[test]
fn an_unreachable_datahub_is_the_connect_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let mut rerolled: Vec<String> = Vec::new();

    for _ in 0..PREMISE_ATTEMPTS {
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
            listener.local_addr().expect("resolve assigned port")
            // ...and the listener is dropped here, so nothing is listening on that port — for as
            // long as nobody else binds it, which is precisely what the probes below measure.
        };
        let text = addr.to_string();

        let mut lost = None;
        for sub in ["ls", "coverage"] {
            if is_listening(&addr) {
                lost = Some(format!("{text} was taken before `{sub}` ran"));
                break;
            }
            let out = run(scratch.path(), &["data", "hist", sub, "--addr", &text]);
            let err = stderr(&out);
            if is_listening(&addr) {
                let code = out.status.code();
                lost = Some(format!("{text} was taken while `{sub}` ran (it exited {code:?})"));
                break;
            }
            // Both probes refused, so the address was closed across the whole run: everything
            // below is the CLI's answer to an unreachable datahub and nothing else.
            assert_eq!(
                out.status.code(),
                Some(3),
                "{sub} must be connect-class — {text} refused a connect both before and after this \
                 run, so the port was not stolen and this is the CLI's own answer: {err}"
            );
            assert!(err.contains("cannot connect to datahub"), "{sub}: {err}");
            assert!(err.contains(&text), "{sub} must name the address: {err}");
            assert_eq!(stdout(&out), "", "{sub} wrote a document for a run that never happened");
        }

        match lost {
            Some(why) => rerolled.push(why),
            // Both rungs proved, on a port measured closed on both sides of both runs.
            None => return,
        }
    }

    panic!(
        "the premise never held: {PREMISE_ATTEMPTS} freshly-bound ephemeral ports were each taken \
         by another process before this case could prove anything about them — {rerolled:?}"
    );
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

    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["subcommand"], "ls");
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

    let out =
        run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--venue", "OKX", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["count"], 1, "case-insensitive substring on the venue");
    assert_eq!(doc["series_reported"], 2, "…beside what the server actually reported");
    assert_eq!(doc["series"][0]["venue"], "okx");
    assert_eq!(doc["filter"]["venue"], "OKX", "the filter is echoed as typed");

    // A filter that matches nothing is a successful run with an honest empty, never a failure.
    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--name", "NOSUCH"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no series match the filter (2 reported)"), "{}", stdout(&out));
}

/// `--gaps` reaches `series_gaps` per matched series, and an EMPTY answer is rendered as an
/// answered "no gaps" rather than as silence — the distinction an operator typed the flag for.
#[test]
fn gaps_are_fetched_per_series_and_an_empty_answer_is_said_out_loud() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--gaps", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["gaps_requested"], true);
    for i in 0..2 {
        let gaps = doc["series"][i]["gaps"].as_array().expect("--gaps was asked for");
        assert!(gaps.is_empty(), "series {i} reported holes it does not have: {gaps:?}");
        assert!(doc["series"][i]["gaps_error"].is_null(), "series {i}");
    }

    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--gaps"]);
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

    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr]);
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

    let out = run(scratch.path(), &["data", "hist", "coverage", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["subcommand"], "coverage");
    assert_eq!(doc["addr"], addr);
    assert_eq!(doc["instruments_reported"], 0);
    assert_eq!(doc["count"], 0);
    assert!(doc["instruments"].as_array().expect("an array, even when empty").is_empty());

    let out = run(scratch.path(), &["data", "hist", "coverage", "--addr", &addr]);
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
        (vec!["data", "hist", "rm"], "--kind"),
        (vec!["data", "hist", "rm", "--kind", "bar"], "--venue"),
        (
            vec![
                "data", "hist", "rm", "--kind", "bar", "--venue", "b", "--symbol", "S", "--group",
                "G",
            ],
            "ALTERNATIVES",
        ),
        (
            vec![
                "data",
                "hist",
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
        (
            vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b", "--symbol", "BTC*"],
            "glob character",
        ),
        (
            vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b", "--symbol", " "],
            "EMPTY value",
        ),
        // ⚠ **The blank PREFIX, in both spellings, on both routes.** There was no row for it —
        // the loop above happened to catch it, with a sentence about the store's grouped-series
        // `symbol=` sentinel that is false of a provenance assertion and names the opposite
        // remedy. The needle is the CONSEQUENCE, so the wrong refusal cannot satisfy it.
        (
            vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b", "--produced-by", ""],
            "matches every key",
        ),
        (
            vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b", "--produced-by="],
            "matches every key",
        ),
        (
            vec![
                "data",
                "hist",
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
                "hist",
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
        (vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b", "--name", "x"], "LISTING"),
        (
            vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b", "--days", "3"],
            "FETCH window",
        ),
        (vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b", "b:S:1h"], "takes no"),
        (
            vec![
                "data", "hist", "rm", "--kind", "bar", "--venue", "b", "--addr", "h:1", "--store",
                "/s",
            ],
            "two DIFFERENT stores",
        ),
        // ⚠ THE ONE. Fully named, so no `--produced-by` is needed, and the command line is
        // otherwise perfect — it is refused because nobody can confirm it.
        (
            vec![
                "data",
                "hist",
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
///
/// ⚠ **`repair` is in this list, and that is the interesting row.** It shares `rm`'s SELECTOR
/// flags, so the two are easy to read as one grammar — but `--produced-by` asserts the commit-key
/// provenance of what is about to be DELETED, and a rebuild deletes nothing. Accepting it there
/// would be a flag that looks like a safety rail in front of an act it does not guard.
#[test]
fn rms_flags_are_refused_on_the_other_subcommands() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for sub in [
        "fetch",
        "fetch-starter",
        "seed-demo",
        "export",
        "ls",
        "coverage",
        "health",
        "universe",
        "repair",
    ] {
        let out = run(scratch.path(), &["data", "hist", sub, "--produced-by", "panel_bars:"]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{sub}: {err}");
        assert!(err.contains("--produced-by"), "{sub} must name the flag: {err}");
        assert!(err.contains("`rm`"), "{sub} must name the verb that takes it: {err}");
    }
}

/// ⚠ **`data repair` REHEARSES by default and refuses the remote route**, proved through the
/// shipped binary rather than only through the parser — the two properties an operator meets
/// first, and the two a refactor of the flag ladder would break silently.
///
/// The spawn half is the same stand-in-engine pattern `rm_reaches_the_engine_as_its_own_flags`
/// uses, and for its reason: a `#!` script is what makes a text file executable, and it keeps this
/// file out of a DataFusion build.
#[cfg(unix)]
#[test]
fn repair_rehearses_by_default_and_has_no_remote_route() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let planted =
        common::plant_engine(scratch.path(), "fake-engine", "#!/bin/sh\necho \"argv: $*\"\n");
    let engine = planted.arg();

    // The BARE form — no --yes, no --dry-run — reaches the child as `--dry-run`. The child is TOLD
    // what this side decided rather than inheriting a default spelled in two places.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "repair",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--interval",
            "1m",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: data repair --kind bar --venue binance --symbol BTCUSDT --interval 1m --dry-run"
    );

    // ...and only `--yes` alone turns it into a write.
    let out = run(
        scratch.path(),
        &[
            "data", "hist", "repair", "--kind", "trade", "--venue", "d", "--symbol", "S", "--yes",
            "--engine", engine,
        ],
    );
    assert!(stdout(&out).trim().ends_with("--yes"), "{}", stdout(&out));
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "repair",
            "--kind",
            "trade",
            "--venue",
            "d",
            "--symbol",
            "S",
            "--yes",
            "--dry-run",
            "--engine",
            engine,
        ],
    );
    assert!(stdout(&out).trim().ends_with("--dry-run"), "--dry-run wins: {}", stdout(&out));

    // The REMOTE route is refused before anything is spawned or dialled, on the usage rung, with
    // the reason that would otherwise be re-litigated.
    let out = run(
        scratch.path(),
        &[
            "data", "hist", "repair", "--kind", "bar", "--venue", "b", "--symbol", "S", "--addr",
            "h:1",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("ENUMERATED"), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "a refusal writes nothing to stdout");
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
    let scratch = tempfile::tempdir().expect("tempdir");
    let planted =
        common::plant_engine(scratch.path(), "fake-engine", "#!/bin/sh\necho \"argv: $*\"\n");
    let engine = planted.arg();

    // A fully-named series, confirmed with --yes: one spawn, and the argv is the product.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
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
            "hist",
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
    let scratch = tempfile::tempdir().expect("tempdir");
    let script = concat!(
        "#!/bin/sh\n",
        "echo '{\"store_root\":\"/srv/hist\",\"store_rung\":\"explicit\",\"matched\":0}'\n"
    );
    let planted = common::plant_engine(scratch.path(), "fake-engine", script);
    let engine = planted.arg();

    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
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
            "hist",
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
