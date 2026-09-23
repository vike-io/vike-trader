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

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::live::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};
use vike_data::{HistStore, MemHistStore};
use vike_datahub::md::MdHub;
use vike_datahub::{serve, serve_authed};
use vike_datahub_client::{
    FEATURE_MARKET_DATA, MdBye, MdFrame, MdSessionId, PROTO_VERSION, Request, Response,
    md_venue_feature, read_frame, write_frame,
};
use vike_model::{AssetClass, Bar, SymbolProperties, TradeTick};
use vike_node_proto::auth::NodeKeys;

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
///
/// ⚠ **The DATAHUB node pair is removed here because the settings redirect CANNOT answer for it.**
/// `crates/vike-cli/src/lib.rs`'s `datahub_keyring` reads `node_keys_from_vars` over the PROCESS
/// ENVIRONMENT first and the node store only second — "The environment still WINS", its own doc —
/// so `VIKE_SETTINGS_DIR` pointed at an empty temp directory does not make a box that EXPORTS the
/// pair look keyless, and that box is the shape that function's doc records as the real the CI box one.
/// One case cares: [`the_capability_matrix_says_which_side_refused_when_a_server_answers_and_says_no`]
/// names `DatahubClient::connect`'s post-handshake refusal as the path it proves, and with the pair
/// exported the run takes `connect_authed` instead, the keyed fixture denies the mac, and the
/// refusal arrives as `Response::AuthDenied`. The verdict is `PermissionDenied` either way, so the
/// case stayed green either way — what was unpinned was the PATH its doc names, which is how a
/// later author deletes the `connect` arm's branch and sees nothing redden.
/// `crates/vike-cli/tests/study_report_refusal_cli.rs` removes the same pair for the same reason.
fn run(settings_dir: &Path, args: &[&str]) -> Output {
    common::output_retrying_etxtbsy(&format!("run vike-cli {args:?}"), || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
        c.args(args)
            .env("VIKE_SETTINGS_DIR", settings_dir)
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_DATAHUB_OBSERVE_KEY")
            .env_remove("VIKE_DATAHUB_CONTROL_KEY");
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
    // `health` now. `fetch-starter` and `seed-demo` are NOT here: they stopped being verbs when
    // the source became an axis, and the help advertises them as `fetch --source starter|demo`,
    // which the two assertions below check by their own spelling.
    for sub in [
        "fetch", "export", "get", "ls", "gaps", "coverage", "health", "universe", "gate", "rm",
        "repair",
    ] {
        assert!(text.contains(sub), "`data --help` must list `{sub}`: {text}");
    }
    for axis in ["--source starter", "--source demo", "--source SRC"] {
        assert!(text.contains(axis), "`data --help` must advertise `{axis}`: {text}");
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
    // ⚠ The floor catches a PARSE bug yielding a short list — not the roster's exact size, which
    // is what the loop below covers. It has moved three times and every move was REAL: the source
    // collapse removed two verbs (`fetch-starter`/`seed-demo` are `fetch --source starter|demo`
    // now), the gaps promotion added one back (`ls --gaps` is `gaps`), and `gate` arrived with the
    // readiness verdict. It sits one BELOW the roster deliberately, so one deliberate removal does
    // not redden it while a truncation — which yields one entry, never nine — still does. Move it
    // only for a real removal.
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
        (vec!["data", "hist", "fetch", "--source", "demo", "--days", "30"], "--days"),
        (vec!["data", "hist", "fetch", "--source", "demo", "--nope"], "unknown option"),
        // The two ruling-12 arrivals: each refuses what it cannot honour, by name.
        (vec!["data", "hist", "export", "--out", "s.parquet"], "needs a spec"),
        (vec!["data", "hist", "export", "demo:D:1h"], "--out"),
        (vec!["data", "hist", "export", "demo:D:1h", "--out", "s", "--days", "7"], "FETCH"),
        (vec!["data", "hist", "fetch", "--source", "starter", "d:S:1h"], "no spec"),
        (vec!["data", "hist", "fetch", "--source", "starter", "--days", "7"], "--days"),
        (vec!["data", "hist", "fetch", "b:S:1h", "--days", "1", "--out", "s"], "--out"),
        // The READ half's own refusals, caught before a socket is opened — see below for why the
        // first of these is the one that matters most.
        (vec!["data", "hist", "ls", "binance:BTCUSDT:1h"], "kind, venue, symbol-or-group"),
        (vec!["data", "hist", "ls", "--partial-only"], "coverage"),
        // ⚠ `--gaps` is refused on EVERY verb now, `ls` included — it is a VERB. The message
        // names the replacement rather than reading as an unknown option.
        (vec!["data", "hist", "ls", "--gaps"], "data hist gaps"),
        (vec!["data", "hist", "coverage", "--gaps"], "data hist gaps"),
        (vec!["data", "hist", "gaps", "--class"], "data hist ls --class"),
        (vec!["data", "hist", "coverage", "--kind", "trade"], "across kinds"),
        // `gate`'s own: no subject, no criterion, a criterion flag on a verb that judges nothing,
        // and a duration this store could not be gated on. Every one is caught before a socket is
        // opened, which is the property that makes a wrong command line cost nothing.
        (vec!["data", "hist", "gate", "--require-days", "30"], "gate needs a spec"),
        (vec!["data", "hist", "gate", "binance:BTCUSDT:1h"], "checked nothing"),
        (vec!["data", "hist", "gate", "BTCUSDT", "--require-days", "1"], "not a series spec"),
        (vec!["data", "hist", "gate", "b:S:1h", "--require-days", "0"], "asserts nothing"),
        (
            vec!["data", "hist", "gate", "b:S:1h", "--require-days", "1", "--max-gap", "3mo"],
            "CALENDAR",
        ),
        (
            vec!["data", "hist", "gate", "b:S:1h", "--require-days", "1", "--kind", "bar"],
            "--require-kind",
        ),
        // ...and the criterion a SPEC can never satisfy: only `bar` series sub-partition by step,
        // so an interval-bearing spec plus a tick kind could only ever breach — over a tape that
        // may well be on disk. Refused as a command-line mistake, like its mirror one row down.
        (
            vec![
                "data",
                "hist",
                "gate",
                "b:S:1h",
                "--require-days",
                "1",
                "--require-kind",
                "trade",
            ],
            "could only ever select nothing",
        ),
        (
            vec!["data", "hist", "gate", "b:@G:1h", "--require-days", "1"],
            "could only ever select nothing",
        ),
        (vec!["data", "hist", "ls", "--require-days", "30"], "data hist gate"),
        (vec!["data", "hist", "coverage", "--max-gap", "1d"], "data hist gate"),
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
        &[
            "data",
            "hist",
            "fetch",
            "--source",
            "demo",
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

    let out =
        run(scratch.path(), &["data", "hist", "fetch", "--source", "demo", "--engine", engine]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "argv: data seed-demo");

    // ⚠ **The two ruling-12 arrivals reach the engine as themselves.** They had no home in this
    // verb at all before, so this is what proves the move LANDED rather than merely being written
    // into a help string: `vike-cli data export …` and `vike-cli data fetch-starter` become the
    // engine subcommand of the same name, and the argv reads as the same words on both sides.
    let out = run(
        scratch.path(),
        &["data", "hist", "fetch", "--source", "starter", "--store", "/s", "--engine", engine],
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

    let out = run(
        scratch.path(),
        &["data", "hist", "fetch", "--source", "demo", "--engine", planted.arg()],
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

    let out = run(
        scratch.path(),
        &["data", "hist", "fetch", "--source", "demo", "--json", "--engine", planted.arg()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    // ⚠ The verb is `fetch` and the SOURCE carries what the old name did — both asserted,
    // because either alone would let the collapse lose a fact a caller used to have.
    assert_eq!(doc["subcommand"], "fetch");
    assert_eq!(doc["source"], "demo");
    assert!(doc["series"].is_null());
    assert!(doc["window"].is_null());
    assert!(doc["store"].is_null(), "no --store was given, so there is no path this side knows");
    // ⚠ ...and the ENGINE still hears its own verb. Our three collapsed into one; its did not.
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

    let out = run(
        scratch.path(),
        &["data", "hist", "fetch", "--source", "demo", "--engine", planted.arg()],
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
    let scratch = tempfile::tempdir().expect("tempdir");
    // A line on stdout BEFORE the failure, so this proves the document is withheld rather than
    // merely that the child printed nothing.
    let planted = common::plant_engine(
        scratch.path(),
        "refusing-engine",
        "#!/bin/sh\necho 'fetching'\necho 'no network fetch in this build' >&2\nexit 2\n",
    );
    let engine = planted.arg();

    let out = run(
        scratch.path(),
        &["data", "hist", "fetch", "--source", "demo", "--json", "--engine", engine],
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
        // ⚠ Whole ARGUMENT LISTS rather than verb names, because `gate` carries its own required
        // flags — and it is in this set for a reason stronger than symmetry. Its rung is its
        // PRODUCT, so a gate that answered `breach` for a socket that never opened would tell a CI
        // step its DATA is bad when its TUNNEL is down, which is the retry-vs-escalate inversion
        // this whole ladder exists to remove.
        for args in [
            vec!["data", "hist", "ls"],
            vec!["data", "hist", "coverage"],
            vec!["data", "hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1"],
        ] {
            let sub = args[2];
            if is_listening(&addr) {
                lost = Some(format!("{text} was taken before `{sub}` ran"));
                break;
            }
            let mut argv = args.clone();
            argv.extend_from_slice(&["--addr", text.as_str()]);
            let out = run(scratch.path(), &argv);
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
            // Every verb in the set proved, on a port measured closed on both sides of each run.
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
    assert!(bars["gaps"].is_null(), "`ls` asks no gap probe, so the field is null not empty");

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

/// `data hist gaps` reaches `series_gaps` per matched series, and an EMPTY answer is rendered as
/// an answered "no gaps" rather than as silence — the distinction the verb exists to draw.
///
/// ⚠ This is also the end-to-end proof of the PROMOTION: the probe now follows the VERB, so the
/// document's `gaps_requested` is `true` with no flag on the line at all, and the same filters
/// still narrow it.
#[test]
fn gaps_are_fetched_per_series_and_an_empty_answer_is_said_out_loud() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "hist", "gaps", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["subcommand"], "gaps");
    assert_eq!(doc["gaps_requested"], true, "the VERB armed the probe, with no flag on the line");
    for i in 0..2 {
        let gaps = doc["series"][i]["gaps"].as_array().expect("the verb asked for them");
        assert!(gaps.is_empty(), "series {i} reported holes it does not have: {gaps:?}");
        assert!(doc["series"][i]["gaps_error"].is_null(), "series {i}");
    }

    let out = run(scratch.path(), &["data", "hist", "gaps", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).matches("no gaps").count(),
        2,
        "each matched series gets an explicit verdict: {}",
        stdout(&out)
    );

    // ...and the filters `ls` takes narrow it identically — the property that let this become a
    // verb without building a series identity out of flags.
    let out = run(scratch.path(), &["data", "hist", "gaps", "--addr", &addr, "--venue", "OKX"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).matches("no gaps").count(),
        1,
        "the venue filter reached the probe: {}",
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

/// **THE ROW VERB, END TO END** — `data hist get` reaches a real datahub's `LoadBars` through
/// `DatahubClient::load_bars_ms` and prints the rows themselves.
///
/// ⚠ **What this proves that no unit test can**: the epoch-ms sibling method genuinely talks to a
/// server built from the shipping protocol, with no wire change — the claim the surface design's
/// §8.2 makes and the whole reason this verb needed no protocol arm. The bounds are asserted by
/// NARROWING as well as by matching, because a verb that dropped them on the floor would answer
/// with all three bars and satisfy every "the rows arrived" assertion.
#[test]
fn get_reaches_the_rows_over_the_epoch_ms_sibling_and_the_window_bounds_the_read() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let last = (2 * DAY_MS).to_string();

    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "get",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--from",
            "0",
            "--to",
            &last,
            "--json",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["subcommand"], "get");
    assert_eq!(doc["addr"], addr);
    assert_eq!(doc["spec"]["text"], "binance:BTCUSDT:1h");
    assert_eq!(doc["returned"], 3, "the whole seeded series is inside this window");
    assert_eq!(doc["shown"], 3);
    assert_eq!(doc["truncated"], false);
    let bars = doc["bars"].as_array().expect("an array of rows");
    assert_eq!(bars.len(), 3);
    assert_eq!(bars[0]["ts"], 0);
    assert_eq!(bars[0]["close"], 1.5, "the PRICE itself, which no sibling verb renders");
    assert_eq!(bars[0]["venue"], "binance", "every row is self-describing");
    assert!(bars[0].get("bid").is_none(), "an unrecorded tick-derived field is OMITTED");

    // THE ANTI-VACUITY CONTROL: the same series over a ONE-DAY window answers with one row, so the
    // bounds above genuinely crossed the wire rather than being parsed and dropped.
    let one = DAY_MS.to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "get",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--from",
            &one,
            "--to",
            &one,
            "--json",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["returned"], 1, "the window narrowed the READ: {}", stdout(&out));
    assert_eq!(doc["bars"][0]["ts"], DAY_MS);
}

/// **§8.2 RULE 1, ON THE SHIPPED BINARY**: hitting the row ceiling is REPORTED with the exact count
/// withheld, and a `--limit` above the ceiling is refused rather than clamped.
#[test]
fn the_row_ceiling_is_disclosed_and_a_limit_above_it_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    // ⚠ `--from 0` rather than `--days N`: a half-open window from the epoch reaches this
    // fixture's bars whatever the wall clock says, so the case pins the CEILING rather than
    // accidentally pinning how far back a day count happens to reach in the year it is run.
    let spec_and_window =
        ["data", "hist", "get", "binance:BTCUSDT:1h", "--addr", &addr, "--from", "0"];

    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", "2"]);
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "a cut answer is a successful run: {}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(
        text.lines().filter(|l| l.contains("1970-01-0")).count(),
        2,
        "cut to --limit: {text}"
    );
    assert!(text.contains("1 more rows"), "the EXACT count withheld is disclosed: {text}");
    assert!(text.contains("--limit 2"), "…and WHICH ceiling cut it: {text}");
    assert!(text.contains("export"), "…and the verb bulk extraction belongs to: {text}");

    // The machine form carries the same fact as FIELDS rather than as a note.
    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", "2", "--json"]);
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["returned"], 3);
    assert_eq!(doc["shown"], 2);
    assert_eq!(doc["truncated"], true);

    // ...and a limit ABOVE the ceiling is a command-line refusal, never a silent clamp — which is
    // the half of the rule an operator would otherwise never learn they had hit.
    //
    // ⚠ **NEITHER the argv NOR a bare `1000` can be what this asserts on, and it shipped asserting
    // on both.** The case read `--limit 100000` + `contains("1000")`, and `100000` CONTAINS
    // `1000` — so the argv echoed back into the refusal satisfied it on its own. So did the USAGE
    // page, which `crate::cmd::args`'s `exit_for_parse_error` prints on the SAME stream for every
    // parse error and which states the ceiling twice (`at most 1000`, `(default 1000)`). Three
    // sources, one of them the thing under test. The value below carries none of the ceiling's
    // digits and the needle is the refusal's own phrase; the two controls pin both hazards.
    const ABOVE_THE_CEILING: &str = "4096";
    const NAMED: &str = "ceiling of 1000 rows";
    assert!(
        !ABOVE_THE_CEILING.contains("1000"),
        "the argv must not itself supply the literal this case asserts on"
    );
    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", ABOVE_THE_CEILING]);
    let out = run(scratch.path(), &argv);
    let refused = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "a refused command line is the usage rung: {refused}");
    assert!(refused.contains(NAMED), "the ceiling is NAMED by the refusal: {refused}");
    assert!(
        refused.contains(ABOVE_THE_CEILING),
        "…and so is what WAS asked for, which is the half that makes a clamp unbelievable: \
         {refused}"
    );
    // THE CONTROL: a DIFFERENT `--limit` refusal, which prints the very same USAGE page on the
    // very same stream, does NOT carry that phrase. Without it the assertion above would still
    // pass off the usage text if the refusal ever stopped naming the number.
    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", "seven"]);
    let out = run(scratch.path(), &argv);
    let other = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "…still a usage rung: {other}");
    assert!(other.contains("whole number"), "a different refusal, same page: {other}");
    assert!(!other.contains(NAMED), "the phrase is the REFUSAL's, not the usage page's: {other}");
}

/// Under `--format jsonl`, STDOUT CARRIES ROWS AND NOTHING ELSE — the property a `| jq` and a
/// `> rows.jsonl` both depend on.
///
/// ⚠ The disclosures still happen; they go to STDERR. A run that simply dropped them would pass a
/// "stdout is only rows" assertion just as well, so both streams are asserted.
#[test]
fn a_jsonl_get_puts_rows_on_stdout_and_its_notes_on_stderr() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "get",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--from",
            "0",
            "--limit",
            "2",
            "--format",
            "jsonl",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let rows = jsonl_rows(&stdout(&out));
    assert_eq!(rows.len(), 2, "one object per row and nothing else on stdout");
    assert_eq!(rows[0]["interval"], "1h");
    assert!(stderr(&out).contains("1 more rows"), "the note went to stderr: {}", stderr(&out));
}

/// An EMPTY answer never claims the series is absent, because the wire cannot tell the two apart —
/// and it is a SUCCESS, like every other honest empty in this plane.
///
/// ⚠ **All THREE renderings, because the one a script reads was the one that said nothing.** The
/// `json` arm shipped printing `returned: 0, bars: []` and exiting 0 with nothing on either
/// stream, while `jsonl` — equally machine-facing — put the note on stderr. A consumer reads that
/// document as "the store has no bars for that week" when the truth is "that series is not in this
/// store", which is the exact misreading the note exists to prevent.
#[test]
fn a_get_that_matches_nothing_states_both_readings_and_still_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let base = ["data", "hist", "get", "binance:NOSUCH:1h", "--addr", &addr, "--from", "0"];

    let out = run(scratch.path(), &base);
    assert!(out.status.success(), "an empty window is a fact, not a failure: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("ONE of two facts"), "{text}");
    assert!(text.contains("data hist ls"), "…and the verb that answers the other: {text}");

    // The DOCUMENT form carries it as a field — stdout stays exactly one JSON document.
    let mut argv = base.to_vec();
    argv.push("--json");
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["returned"], 0);
    let note = doc["note"].as_str().unwrap_or_else(|| panic!("a `note` field: {doc}"));
    assert!(note.contains("ONE of two facts"), "the ambiguity is a FIELD: {note}");
    assert!(note.contains("data hist ls"), "…naming the verb that answers the other: {note}");

    // The SEQUENCE form carries it on stderr, because stdout is rows and only rows.
    let mut argv = base.to_vec();
    argv.extend(["--format", "jsonl"]);
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "", "no rows means no stdout at all under jsonl");
    assert!(stderr(&out).contains("ONE of two facts"), "{}", stderr(&out));

    // THE CONTROL: a series the fixture DOES hold carries no note in any of the three, so every
    // assertion above is about the emptiness rather than about a sentence printed unconditionally.
    let held = ["data", "hist", "get", "binance:BTCUSDT:1h", "--addr", &addr, "--from", "0"];
    let out = run(scratch.path(), &held);
    assert!(!stdout(&out).contains("ONE of two facts"), "{}", stdout(&out));
    let mut argv = held.to_vec();
    argv.push("--json");
    let out = run(scratch.path(), &argv);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert!(doc["returned"].as_u64().is_some_and(|n| n > 0), "the fixture holds this one: {doc}");
    assert!(doc.get("note").is_none(), "a note on every run stops being read: {doc}");
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

/// The group ANSWERS now, and its help is the only place its four verbs are named.
///
/// ⚠ This is also the case that would catch `catalog` being routed back to the unbuilt-group stub:
/// that path exits on the USAGE rung with "designed but not built yet", and this asserts the
/// opposite of both halves.
///
/// ⚠ **The verb check is over the ROW, not over the word, and that is a correction.** It asserted
/// `text.contains(verb)` and could not fail for the reason it names: every verb name also occurs
/// in the page's own PROSE (`--venue V   ls/refresh:`, "`data catalog venues` is the roster",
/// "an instrument `ls` lists"), so deleting a verb's whole usage block left this green with the
/// verb undiscoverable from `--help` — measured in the unit suite beside
/// `crates/vike-cli/src/cmd/data/catalog.rs`'s `usage`, where `ls` occurred 9 times and `refresh`
/// twice outside their own blocks. A usage BLOCK is the only line that opens at column 2 with the
/// verb and carries text after it.
#[test]
fn the_catalog_group_answers_and_its_help_names_every_verb() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "catalog", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for verb in ["ls", "show", "refresh", "venues"] {
        let head = format!("  {verb} ");
        let has_block = text.lines().any(|l| match l.strip_prefix(&head) {
            Some(rest) => !rest.trim().is_empty(),
            None => false,
        });
        assert!(has_block, "`data catalog --help` must carry `{verb}`'s own block: {text}");
    }
    assert!(!text.contains("designed but not built"), "this group is built: {text}");

    // ...and a bare group names its roster rather than doing something by default.
    let bare = run(scratch.path(), &["data", "catalog"]);
    assert_eq!(bare.status.code(), Some(2), "a verb is REQUIRED: {bare:?}");
    let err = stderr(&bare);
    for verb in ["ls", "show", "refresh", "venues"] {
        assert!(err.contains(verb), "the refusal must name `{verb}`: {err}");
    }
    assert!(!err.contains("designed but not built"), "{err}");
}

/// **§8.1's load-bearing requirement, as a spawned process: the matrix answers with NO datahub.**
///
/// A box that cannot reach one is exactly the box whose operator needs to know what this build can
/// do, so an unreachable server is REPORTED and the verb still exits 0 with every roster venue on
/// stdout. The roster is read from `vike_model::VENUES` here rather than typed, for the same reason
/// the module derives it: a venue added to the tree must appear with no edit to this file.
///
/// ⚠ The address is a port this test BOUND and then released, and the premise is MEASURED on both
/// sides of the run — the mechanism (and the incident) [`an_unreachable_datahub_is_the_connect_rung`]
/// carries. Here it guards only the "NOT REACHED" half: the exit code and the venue rows are
/// asserted unconditionally, because `venues` owes 0 and a full local matrix whoever is on that
/// port.
#[test]
fn the_capability_matrix_answers_with_no_datahub_reachable() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let mut rerolled: Vec<String> = Vec::new();

    for _ in 0..PREMISE_ATTEMPTS {
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
            listener.local_addr().expect("resolve assigned port")
        };
        let text = addr.to_string();
        if is_listening(&addr) {
            rerolled.push(format!("{text} was taken before the run"));
            continue;
        }
        let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &text]);
        let body = stdout(&out);
        // Unconditional: whoever holds that port, this verb owes a local matrix and a zero.
        assert!(out.status.success(), "`venues` must not fail on an absent datahub: {out:?}");
        for venue in vike_model::VENUES {
            assert!(body.contains(venue), "every roster venue must be rendered: {body}");
        }
        if is_listening(&addr) {
            rerolled.push(format!("{text} was taken while the run happened"));
            continue;
        }
        // Both probes refused, so the address was closed across the whole run and the server half
        // of the rendering is the CLI's own answer to an absent datahub.
        assert!(body.contains("NOT REACHED"), "the missing half must be SAID: {body}");
        assert!(
            body.contains("unasked, which is not the same as unserved"),
            "an unasked column may not be reported as a refusal: {body}"
        );
        assert!(!body.contains("not served"), "nothing may claim that server refused: {body}");
        return;
    }

    panic!(
        "the premise never held: {PREMISE_ATTEMPTS} freshly-bound ephemeral ports were each taken \
         by another process before this case could prove anything about them — {rerolled:?}"
    );
}

/// The matrix against a REAL datahub keeps the two sources APART, which is the whole of §8.1.
///
/// The seeded server is `vike_datahub::serve`, which mounts no catalog lane and no market-data hub
/// — so it advertises neither `venue_catalog` nor any `md_venue=` entry. That is not a degenerate
/// fixture, it is the commonest deployment: a data daemon serving store reads. What it proves is
/// that the build's columns survive a server that serves none of them, which is the failure mode an
/// operator cannot otherwise see.
#[test]
fn the_capability_matrix_separates_this_build_from_the_server_that_answered() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["group"], "catalog");
    assert_eq!(doc["verb"], "venues");
    // The BUILD half: every roster venue, from this binary's own tables.
    let venues = doc["build"]["venues"].as_array().expect("the build's rows");
    assert_eq!(venues.len(), vike_model::VENUES.len());
    // The SERVER half: a real handshake, kept in its own object. ⚠ `state` is a THREE-token field
    // where this read `reachable`, a boolean — a server that answers the handshake and then
    // REFUSES the connection (a PROTO_VERSION skew, a key it will not take) was reached, and
    // `"reachable": false` said the opposite. See `ServerView::state`.
    assert_eq!(doc["server"]["state"], "answered");
    assert!(doc["server"]["features"].is_array(), "a reachable server carries its features: {doc}");
    assert_eq!(
        doc["server"]["serves_venue_catalog"], false,
        "this fixture mounts no catalog lane, and the document must say so rather than omit it"
    );
    // ...and the DIFFERENCE, named rather than folded into either half. This server advertises no
    // `md_venue=` entry at all, so every venue this build declares a live feed for is skew.
    let declared = doc["skew"]["declared_here_unserved_there"].as_array().expect("the skew");
    assert!(!declared.is_empty(), "this build declares live feeds nobody here serves: {doc}");
    // The anti-vacuity control: the build's own live-lane column is UNTOUCHED by that skew, which
    // is the merge §8.1 forbids. A venue named in the skew still carries its lanes above it.
    let skewed = declared[0].as_str().expect("a venue slug");
    let row = venues
        .iter()
        .find(|v| v["venue"] == skewed)
        .unwrap_or_else(|| panic!("{skewed} must have a build row: {doc}"));
    assert!(
        !row["live_lanes"].as_array().expect("lanes").is_empty(),
        "the server's `no` must not empty this build's column: {row}"
    );

    // ...and the same run in TABLE mode, for the sentence the document does not carry.
    //
    // ⚠ That sentence used to end `a `data realtime watch` on one of them is refused by the
    // server, not by this binary`. When it was struck, both halves were false because that group
    // had no route at all — it was refused on this binary's own usage rung. `data realtime watch`
    // SHIPS now, so the first half is true and the second is still false: this fixture's datahub
    // advertises no market-data plane, so `DatahubClient::md_subscribe` refuses on the capability
    // LOCALLY, "nothing was sent", and the refusal the sentence attributed to the server never
    // reaches it. The one verb whose product is keeping "which side said no" legible would have
    // been naming the wrong side. This is the case that catches it coming back, end to end.
    let table = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr]);
    assert!(table.status.success(), "{}", stderr(&table));
    let body = stdout(&table);
    assert!(body.contains("SKEW"), "this fixture serves no md_venue, so there IS a skew: {body}");
    assert!(
        !body.contains("data realtime"),
        "the skew line may not attribute a refusal to a side that did not make it: {body}"
    );
}

/// `ls` against a datahub that mounts no catalog lane says WHICH side refused, and does it without
/// sending anything.
///
/// ⚠ The rung is the run-failure one rather than connect-class: the socket was fine and the
/// capability was missing, which is a served answer. `crates/vike-cli/src/cmd/data.rs`'s `connect`
/// is where that split is made and this is the case that would catch it moving.
#[test]
fn a_listing_against_a_server_with_no_catalog_lane_names_the_missing_capability() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out =
        run(scratch.path(), &["data", "catalog", "ls", "--venue", "binance", "--addr", &addr]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "a server that ANSWERED is the run-failure rung: {err}");
    assert!(err.contains("venue_catalog"), "the refusal names the missing capability: {err}");
    assert_eq!(stdout(&out), "", "nothing was printed as though a venue had been listed");
}

/// **`show` reads the STORE, end to end** — a venue producer's recorded grid travels
/// store → `properties_as_of` → wire → the operator — and an instrument with no recorded grid is
/// the EMPTY rung rather than a zeroed one.
///
/// ⚠ The pairing is the point. `okx/BTC-USDT-SWAP` in [`spawn_class_probe_datahub`] carries TWO
/// properties rows and the LATER one names a class, so a `show` that took the first would render
/// `unclassified` and this case would redden — which is what makes the as-of instant assertable
/// from outside the process. `binance/BTCUSDT` in the same fixture has bars and no properties row
/// at all, so it is the absence, and the two must not share an exit code.
#[test]
fn show_reads_the_recorded_grid_and_an_unrecorded_instrument_is_the_empty_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_class_probe_datahub().to_string();

    let args = ["data", "catalog", "show", "okx:BTC-USDT-SWAP", "--addr", &addr, "--json"];
    let out = run(scratch.path(), &args);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["recorded"], true);
    assert_eq!(doc["venue"], "okx");
    assert_eq!(doc["symbol"], "BTC-USDT-SWAP");
    assert_eq!(
        doc["asset_class"],
        AssetClass::CryptoPerp.sql_word(),
        "the LATER row's class, as the model's own stored word: {doc}"
    );
    assert!(
        doc["source"].as_str().unwrap_or_default().contains("kind=properties"),
        "the document must name the source it read, not imply a venue query: {doc}"
    );

    // The absence. A venue-recorded grid and NO grid must not share an exit code, or a pipeline
    // reads "this instrument has a zero tick size" for "nobody has ever recorded it".
    let missing =
        run(scratch.path(), &["data", "catalog", "show", "binance:BTCUSDT", "--addr", &addr]);
    assert_eq!(missing.status.code(), Some(7), "nothing was evaluated: {}", stderr(&missing));
    let err = stderr(&missing);
    assert!(err.contains("RECORDER"), "the absence is a store fact, and says so: {err}");
    assert!(err.contains("data catalog ls"), "…and names the verb that asks the VENUE: {err}");
}

/// A command line this group can refuse locally exits on the USAGE rung, before a socket is opened
/// — so the diagnostic comes from the binary the operator typed rather than from a timeout.
///
/// ⚠ **The VENUE-SHAPE rows are the ones that were missing, and their absence hid a real defect.**
/// This table carried no case for the `--venue` VALUE, and until
/// `crates/vike-cli/src/cmd/data/catalog.rs`'s `parse` called
/// `vike_datahub_client::catalog::validate_catalog_venue`, none of `--venue BINANCE`, `--venue
/// bin@nce` or an empty `--venue` was refused locally at all: each opened a connection, and what
/// the operator was told depended on who was on the port — the CONNECT rung (3, which a wrapper
/// retries) with no datahub up, or the `does not advertise venue_catalog` message against a server
/// with no catalog lane. Neither ever mentioned the slug. These rows are the doc's own claim,
/// applied to the one flag value this group can judge for itself.
///
/// ⚠ The empty spellings are written as `""` on purpose: they are what a shell variable that
/// expanded to nothing produces, which is the case an operator cannot see in their own scrollback.
#[test]
fn a_bad_catalog_command_line_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data", "catalog", "frobnicate"], "unknown `data catalog` verb"),
        (vec!["data", "catalog", "ls"], "--venue"),
        (vec!["data", "catalog", "show"], "VENUE:SYMBOL"),
        (vec!["data", "catalog", "show", "binance:BTCUSDT:1h"], "INTERVAL"),
        (vec!["data", "catalog", "venues", "--venue", "binance"], "roster"),
        (vec!["data", "catalog", "ls", "--venue", "binance", "--class", "perp"], "unknown"),
        // ⚠ The needle was `"not built"` until `export --addr --format csv` shipped. `csv` is
        // WRITTEN now — by a verb, to a file — so the refusal here is about this verb printing
        // rather than about the workspace lacking a writer, and the needle follows the fact.
        (vec!["data", "catalog", "venues", "--format", "csv"], "FILE format"),
        // The venue SHAPE, judged here rather than at the far end of a socket.
        (vec!["data", "catalog", "ls", "--venue", "BINANCE"], "lowercase letters and digits"),
        (vec!["data", "catalog", "refresh", "--venue", "bin@nce"], "outside the permitted set"),
        (vec!["data", "catalog", "ls", "--venue", ""], "names no venue"),
        // ...and the third narrowing/rendering flag that took an empty value in silence.
        (vec!["data", "catalog", "ls", "--venue", "binance", "--search", ""], "EMPTY"),
    ] {
        let out = run(scratch.path(), &args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?} is the USAGE rung: {err}");
        assert!(err.contains(needle), "{args:?} must say `{needle}`: {err}");
        assert_eq!(stdout(&out), "", "{args:?} wrote a document for a run that never happened");
    }
}

/// **THE DERIVED CROSS-CHECK, across two processes.** `ls --json` names the sources; every row it
/// calls `designed` is then driven at the REAL `--source` axis and must be refused by name. So the
/// listing cannot advertise a state the axis disagrees with, and neither side's roster is written
/// down in this file — an integration test cannot see the module's private consts, and a literal
/// here would be the subtract-only failure `help_names_every_subcommand_and_exits_zero` documents.
///
/// ⚠ The `built` half is the anti-vacuity control, and it is driven too: `--source demo` reaches
/// the ENGINE and fails on a missing one (exit 3), which proves the axis did not refuse the value.
/// Without it, "every designed source is refused" would pass on a listing that called all nine
/// designed.
#[test]
fn source_ls_is_a_roster_the_axis_agrees_with() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "ls", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout under --json is the document, whole");
    let rows = doc["sources"].as_array().expect("a sources array");

    let designed: Vec<&str> = rows
        .iter()
        .filter(|r| r["state"] == "designed")
        .map(|r| r["name"].as_str().expect("a name"))
        .collect();
    let built: Vec<&str> = rows
        .iter()
        .filter(|r| r["state"] == "built")
        .map(|r| r["name"].as_str().expect("a name"))
        .collect();
    assert!(!designed.is_empty(), "the listing must carry the designed half: {doc}");
    assert!(!built.is_empty(), "…and the built half: {doc}");

    for name in &designed {
        let fetch = run(
            scratch.path(),
            &["data", "hist", "fetch", "binance:BTCUSDT:1h", "--days", "1", "--source", name],
        );
        let err = stderr(&fetch);
        assert_eq!(fetch.status.code(), Some(2), "`--source {name}` is a usage refusal: {err}");
        // ⚠ The FLAG SPELLING, not the bare name — and the difference is that the bare name could
        // not fail for the `vike` row. Every refusal this binary writes is prefixed
        // `vike-cli data: `, so `err.contains("vike")` was satisfied by the binary's own name
        // whatever the message said; the refusal could have stopped interpolating the value
        // entirely and this assertion would still have passed off the prefix.
        assert!(
            err.contains(&format!("--source {name}")),
            "…naming the VALUE that was refused, not just the plane: {err}"
        );
        assert!(err.contains("not built yet"), "…and never as a spelling mistake: {err}");
        // ...and the listing's own cells say the same thing the axis just said.
        let row = rows.iter().find(|r| r["name"] == *name).expect("the row we came from");
        assert_eq!(row["reaches"], serde_json::Value::Null, "a designed source reaches nothing");
        assert!(row["cost"].as_str().is_some_and(|c| !c.is_empty()), "{row}");
    }

    // THE CONTROL. `demo` is listed as built, so the axis must ACCEPT it — the run gets as far as
    // looking for the engine and fails on the connect rung, which no refused value ever reaches.
    assert!(built.contains(&"demo"), "the built half must name `demo`: {doc}");
    let absent = scratch.path().join("no-such-engine");
    let ok = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "fetch",
            "--source",
            "demo",
            "--engine",
            absent.to_str().expect("utf-8 temp path"),
        ],
    );
    let err = stderr(&ok);
    assert_eq!(ok.status.code(), Some(3), "a built source is not a usage refusal: {err}");
    assert!(!err.contains("not built yet"), "{err}");
}

/// **THE HONEST BOUNDARY, over the shipped binary.** `show` describes a source and says, in its own
/// output, that it verified nothing — because it cannot: this binary links no HTTP client, so the
/// unauthenticated manifest read §11 puts on this verb is not available in this phase.
///
/// ⚠ The `--json` half is the one that matters for a wrapper: `verified_against_the_vendor` is a
/// FIELD rather than a sentence, so a consumer folding this document cannot mistake a local
/// description for a probe of what its key reaches. The URL assertion is the third leg — all three
/// of that source's bases are overridable where their lane is configured and this side resolves
/// none of them, so printing one would name a base this box may not be using.
#[test]
fn source_show_describes_without_ever_claiming_a_probe() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for name in ["vike", "demo", "binance"] {
        let out = run(scratch.path(), &["data", "source", "show", name]);
        assert!(out.status.success(), "{}", stderr(&out));
        let text = stdout(&out);
        assert!(text.contains("nothing above was read"), "`show {name}`: {text}");
        assert!(!text.contains("https://"), "`show {name}` resolved no base: {text}");

        let out = run(scratch.path(), &["data", "source", "show", name, "--json"]);
        assert!(out.status.success(), "{}", stderr(&out));
        let doc: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("stdout under --json is the document");
        assert_eq!(doc["source"], name);
        assert_eq!(
            doc["verified_against_the_vendor"],
            serde_json::Value::Bool(false),
            "`show {name} --json` must state the limit as a field: {doc}"
        );
    }

    // `vike` is the one row §9.1/§9.2 are about: TWO classes, kept apart, and the ruling that it
    // serves no CEX market data stated outright rather than left to be inferred.
    let out = run(scratch.path(), &["data", "source", "show", "vike"]);
    let text = stdout(&out);
    assert!(text.contains("TWO CLASSES"), "{text}");
    assert!(text.contains("NO CEX market data"), "{text}");
    let out = run(scratch.path(), &["data", "source", "show", "vike", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one document");
    assert_eq!(doc["holds"].as_array().map(Vec::len), Some(2), "two classes, separately: {doc}");
    // The control: a source with nothing to separate carries an EMPTY `holds`, so the assertion
    // above is about that row rather than about the field existing at all.
    let out = run(scratch.path(), &["data", "source", "show", "demo", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one document");
    assert_eq!(doc["holds"].as_array().map(Vec::len), Some(0), "{doc}");
}

/// `--addr` is refused BY NAME on a group that reaches no server, on the USAGE rung — and another
/// flag is a DIFFERENT answer, so the refusal is about this flag rather than about every flag.
///
/// ⚠ **The second half used to assert `unknown option`, and the shipped binary no longer says it.**
/// `data source ls --store /srv/vike/data` printed `unknown option '--store'` — a lie by the
/// module's own standard, since `--store` is a real, documented `data hist` flag, and one that sent
/// an operator to check a spelling that was right. Every `--` token this group does not take is now
/// refused as a flag that belongs ELSEWHERE, so this case drives a real sibling flag rather than an
/// invented one.
#[test]
fn source_refuses_addr_by_name_and_a_sibling_groups_flag_differently() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "ls", "--addr", "127.0.0.1:7878"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "a fixable command line is the usage rung: {err}");
    assert!(err.contains("--addr"), "{err}");
    assert!(err.contains("reaches no server"), "the refusal must say WHY: {err}");
    assert!(err.contains("data hist"), "…and name what does take one: {err}");
    assert_eq!(stdout(&out), "", "nothing was printed as though it had run");

    let out = run(scratch.path(), &["data", "source", "ls", "--store", "/srv/vike/data"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2));
    let sentence = err.lines().next().unwrap_or_default();
    assert!(sentence.contains("--store"), "{sentence}");
    assert!(
        !sentence.contains("unknown"),
        "`--store` is a real `data hist` flag, so calling it unknown is the lie: {sentence}"
    );
    assert!(sentence.contains("data hist"), "…and the refusal must say where it belongs: {err}");
    assert!(!sentence.contains("reaches no server"), "…which is `--addr`'s own answer: {sentence}");

    // ⚠ THE OTHER FACE OF THE SAME LIE, and the control this pair needs. The refusal above used to
    // answer EVERY `--` token, so a TYPO was told it was spelt correctly and belonged to a sibling.
    // `--stroe` is a flag nowhere in this binary; it must be refused WITHOUT that claim, or the
    // operator is sent to `data hist` to type it again.
    let out = run(scratch.path(), &["data", "source", "ls", "--stroe", "/srv/vike/data"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2));
    let sentence = err.lines().next().unwrap_or_default();
    assert!(sentence.contains("--stroe"), "{sentence}");
    assert!(
        sentence.contains("not a `data hist` flag either"),
        "a typo must not be told it belongs to a sibling group: {sentence}"
    );
    assert!(
        !sentence.contains("it belongs there"),
        "…which is the claim that made this a lie: {sentence}"
    );
}

/// The ROW a token heads in a help page, or `None`.
///
/// ⚠ It exists because `text.contains(verb)` over a whole help page cannot fail for the reason the
/// test below names. `ls` is a substring of `jsonl` in the `--format` row, of `` `ls` `` in the
/// `--json` prose and of the word `false`; `show` is a substring of "For `show`". Deleting either
/// verb's ROW left `data source --help` advertising neither verb and every assertion green. A row
/// is found by its HEAD — the token at the start of an indented line — which only that row can
/// satisfy.
fn advertised_row<'a>(text: &'a str, token: &str) -> Option<&'a str> {
    text.lines()
        .map(str::trim_start)
        .find(|line| line.strip_prefix(token).is_some_and(|r| r.starts_with(char::is_whitespace)))
}

/// The group's own help is a SUCCESS on stdout — the shared `HELP_SENTINEL` path — and it gives
/// each verb a ROW of its own, which is the only place they are advertised.
#[test]
fn source_help_names_both_verbs_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for verb in ["ls", "show"] {
        assert!(
            advertised_row(&text, verb).is_some(),
            "`data source --help` must give `{verb}` a row of its own: {text}"
        );
    }
    assert!(text.contains("verified"), "…and the limit the group is built around: {text}");
    // THE CONTROL: a verb this group does not have heads no row, so the assertions above are about
    // the rows rather than about the page being long enough to contain any short string.
    assert!(advertised_row(&text, "fetch").is_none(), "a `data hist` verb is not on this page");

    // ...and a group with no verb RENDERS that roster rather than restating it. ⚠ The SENTENCE, not
    // the whole stream: `exit_for_parse_error` prints the usage after the message, and the usage
    // names every verb — so a refusal that named none of them would have passed off the help text
    // printed underneath it.
    let out = run(scratch.path(), &["data", "source"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "{err}");
    let sentence = err.lines().next().unwrap_or_default();
    assert!(sentence.contains("needs a verb"), "{sentence}");
    for verb in ["ls", "show"] {
        assert!(sentence.contains(verb), "the refusal itself must name `{verb}`: {sentence}");
    }
}

/// **THE HONEST BOUNDARY ON THE OTHER VERB.** `ls` says, in BOTH renderings, that it read nothing
/// — and the document says it as the same testable FIELD `show` carries.
///
/// ⚠ It did not. The disclaimer was pushed by `show` alone, so the verb that prints a column headed
/// REACHES — `a datahub`, `the engine, on this box` — carried no statement anywhere that nothing
/// had been asked, and `ls --json` carried `count`/`sources`/`notes` with no honesty field at all.
/// A wrapper folding it read `{"name":"starter","reaches":"the engine, on this box"}` and reported
/// that the starter lane was reachable FROM THIS BOX. It is a fact about the build.
#[test]
fn source_ls_says_it_read_nothing_either() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "ls"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("nothing above was read"), "`ls` must name the limit: {text}");
    assert!(text.contains("REACHES"), "…on the verb that prints that column: {text}");

    let out = run(scratch.path(), &["data", "source", "ls", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one document");
    assert_eq!(
        doc["verified_against_the_vendor"],
        serde_json::Value::Bool(false),
        "the roster document must carry the same field its sibling verb carries: {doc}"
    );
    // ...and every note it documents is a note the table prints, which is what makes `notes` mean
    // one thing across this group's two verbs.
    for note in doc["notes"].as_array().expect("an array of footnotes") {
        let note = note.as_str().expect("a footnote is a sentence");
        assert!(text.contains(note), "`ls` must print the note it documents: {note}");
    }
}

/// One fill for the SHARED-store fixture. The numbers are irrelevant; what matters is that the
/// row lands under `kind=exec_fill` at the SAME `(venue, symbol)` as the bars.
fn fill(ts: i64) -> vike_data::ExecFillRow {
    vike_data::ExecFillRow {
        ts,
        trade_id: "t-1".to_string(),
        client_order_id: "c-1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        px: 100.0,
        commission: 0.0,
        mark_price: None,
        liquidity_side: String::new(),
        commission_asset: String::new(),
    }
}

/// A datahub over a SHARED store — market data and this account's own activity under ONE
/// `(venue, symbol)`, which is the shape a real box has: the account plane writes beside the
/// market plane in the same tree.
///
/// A THIRD fixture rather than a widening of [`spawn_seeded_datahub`], for
/// [`spawn_class_probe_datahub`]'s stated reason: two of this file's cases pin that store's series
/// COUNT, and an extra series would break them for a reason having nothing to do with what they
/// test.
fn spawn_shared_account_store_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = MemHistStore::new();
    store
        .append_bars("binance", "BTCUSDT", "1h", &[bar(0), bar(DAY_MS), bar(2 * DAY_MS)], None)
        .expect("seed bars");
    store.append_exec_fills("binance", "BTCUSDT", &[fill(DAY_MS)], None).expect("seed a fill");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// **The verb whose product is the exit code**, over the three outcomes that must never share a
/// number: held, breached, and nothing evaluated.
///
/// ⚠ The load-bearing row is the THIRD. A spec the store matches nowhere is `7`, not `0` — a gate
/// that answered "pass" for a series it never found is the green-means-nothing-ran failure this
/// ladder exists against, and it is the exact shape a typo in a CI step produces.
#[test]
fn the_gate_answers_a_store_with_one_of_three_rungs() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let gate = |args: &[&str]| {
        let mut v = vec!["data", "hist", "gate"];
        v.extend_from_slice(args);
        v.extend_from_slice(&["--addr", addr.as_str()]);
        run(scratch.path(), &v)
    };

    let held = gate(&["binance:BTCUSDT:1h", "--require-days", "2"]);
    assert_eq!(held.status.code(), Some(0), "two days are there: {}", stderr(&held));
    assert!(stdout(&held).contains("PASS"), "{}", stdout(&held));

    let breached = gate(&["binance:BTCUSDT:1h", "--require-days", "365"]);
    assert_eq!(
        breached.status.code(),
        Some(6),
        "a DECLARED THRESHOLD was breached — the command WORKED: {}",
        stderr(&breached)
    );

    let nothing = gate(&["binance:NOSUCHSYMBOL", "--require-days", "1"]);
    assert_eq!(
        nothing.status.code(),
        Some(7),
        "a spec matching nothing is NOT a pass: {}",
        stderr(&nothing)
    );
    let err = stderr(&nothing);
    assert!(err.contains("nothing to gate"), "{err}");
    assert!(err.contains("this is not a pass"), "…and says so outright: {err}");
    assert!(err.contains("data hist ls"), "…and names the verb that shows the spelling: {err}");
    assert_eq!(stdout(&nothing), "", "there were no criteria, so there is no document");
}

/// ⚠ **The document reaches STDOUT on a BREACH**, which is this verb's one departure from the rule
/// every sibling in `data` follows (a failure is a sentence on stderr, and stdout carries nothing).
/// A breach is not a failure: §7.1 of the backtest surface design requires the verdict to name
/// every criterion that passed and failed, and a CI step handed only the rung would have to re-run
/// the gate to learn which criterion moved.
#[test]
fn a_breaching_gate_still_prints_the_whole_verdict() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "hist", "gate", "binance:BTCUSDT:1h", "--require-days", "365", "--addr", &addr],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("gate binance:BTCUSDT:1h"), "the subject is named: {text}");
    assert!(text.contains("CRITERION") && text.contains("VERDICT"), "{text}");
    assert!(text.contains("require-days"), "the criterion that failed is NAMED: {text}");
    assert!(text.contains(">=365d"), "…with what was asked for: {text}");
    assert!(text.contains("2d"), "…and what the store actually holds: {text}");
    assert!(text.contains("require-kind"), "the PASSING criterion is rendered too: {text}");
    assert!(text.contains("BREACH"), "{text}");
    // ⚠ The DISCLOSURE, which is the difference between a gate that checked one half and a gate
    // that reads as though it checked both. No `--max-gap` was given, so the holes were not looked
    // at, and a verdict that did not say so would be a green over a store with a hole in it.
    assert!(text.contains("HOLES"), "a half-checked gate must say so: {text}");
}

/// A REQUIRED KIND the store does not hold is a BREACH — the operator declared it required — and
/// the row names the kinds this spec DOES hold, so the next command is obvious.
///
/// ⚠ It is emphatically NOT the nothing-was-evaluated rung. That one means *you named a series
/// this store has never heard of*; this one means *the instrument is here and the tape you need is
/// not*. Two different actions, so two different numbers, and the case above asserts the other.
#[test]
fn a_required_kind_the_store_lacks_breaches_and_names_what_is_there() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--require-kind",
            "trade",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("require-kind"), "{text}");
    assert!(text.contains("trade"), "the kind that is missing: {text}");
    assert!(text.contains("this spec holds: bar"), "…and the kind that IS there: {text}");
}

/// `--max-gap` is EVALUATED rather than merely accepted: the probe runs, the criterion renders its
/// own answer, and a tolerance finer than the store's own resolution says so.
///
/// ⚠ The second half is a MEASUREMENT of the store, not a style note. A hole is derived from the
/// `date=` partition set, so the smallest one that can be reported is a whole UTC day — an
/// operator who writes `--max-gap 4h` believing they tolerate a four-hour outage is believing
/// something this store cannot express. The `1d` run is the control: it carries no such note, so
/// the note cannot be passing by always firing.
#[test]
fn a_gap_tolerance_is_evaluated_and_a_sub_day_one_says_the_store_cannot_answer_that_finely() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let gate = |max_gap: &str| {
        run(
            scratch.path(),
            &[
                "data",
                "hist",
                "gate",
                "binance:BTCUSDT:1h",
                "--require-days",
                "2",
                "--max-gap",
                max_gap,
                "--addr",
                &addr,
            ],
        )
    };

    let coarse = gate("1d");
    assert_eq!(coarse.status.code(), Some(0), "{}", stderr(&coarse));
    let text = stdout(&coarse);
    assert!(text.contains("max-gap"), "the criterion is rendered: {text}");
    assert!(text.contains("no gaps"), "…with the probe's own answer: {text}");
    assert!(!text.contains("HOLES"), "the holes WERE checked, so no disclosure: {text}");
    assert!(!text.contains("whole UTC day"), "a day-wide tolerance is answerable: {text}");

    let fine = gate("4h");
    assert_eq!(fine.status.code(), Some(0), "{}", stderr(&fine));
    let text = stdout(&fine);
    assert!(text.contains("whole UTC day"), "{text}");
    assert!(text.contains("no missing day at all"), "…and what it therefore means: {text}");
}

/// The `--json` verdict: the same criteria the table renders, plus the EVIDENCE each one was
/// derived from, so a consumer re-derives a judgement rather than trusting it.
///
/// ⚠ The document carries no note and no prose disclosure — it carries the FACTS both notes are
/// derived from (a null `max_gap_ms`, and each series' own numbers), which is this module's
/// standing rule: a note appended to a document a program parses is noise at best.
#[test]
fn the_gate_document_carries_every_criterion_and_the_numbers_behind_it() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "365",
            "--json",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["subcommand"], "gate");
    assert_eq!(doc["verdict"], "breach");
    assert_eq!(doc["spec"]["text"], "binance:BTCUSDT:1h");
    assert_eq!(doc["spec"]["venue"], "binance");
    assert_eq!(doc["spec"]["grouped"], false);
    assert_eq!(doc["require_days"], 365);
    assert!(doc["max_gap_ms"].is_null(), "the holes were not asked about: {doc}");
    assert_eq!(doc["require_kinds"][0], "bar", "defaulted rather than absent");
    assert_eq!(doc["series_reported"], 2, "the fixture's whole store");
    assert_eq!(doc["series_matched"], 1);
    assert_eq!(doc["series_judged"], 1);

    let criteria = doc["criteria"].as_array().expect("an array of criteria");
    assert_eq!(criteria.len(), 2, "one presence criterion and one days criterion: {criteria:?}");
    let by = |name: &str| -> serde_json::Value {
        criteria
            .iter()
            .find(|c| c["criterion"] == name)
            .unwrap_or_else(|| panic!("{name} is a criterion: {doc}"))
            .clone()
    };
    assert_eq!(by("require-days")["verdict"], "breach");
    assert!(by("require-days")["why"].is_null(), "nothing went unevaluated: {doc}");
    assert_eq!(by("require-kind")["verdict"], "pass");

    // The EVIDENCE — the store's own numbers, so the verdict above can be re-derived rather than
    // trusted. `gaps` is null because no probe was made, never `[]`, which would say "no holes".
    let series = doc["series"].as_array().expect("an array of series");
    assert_eq!(series.len(), 1);
    assert_eq!(series[0]["kind"], "bar");
    assert_eq!(series[0]["interval"], "1h");
    assert_eq!(series[0]["rows"], 3);
    assert_eq!(series[0]["span_days"], 2, "365 was asked for and 2 is what is there");
    assert!(series[0]["gaps"].is_null(), "an unmade probe is null: {}", series[0]);
    assert!(series[0]["gaps_error"].is_null(), "{}", series[0]);

    // ...and a PASSING gate emits the SAME shape, so a CI step that parses one on success is not
    // parsing something else on failure.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "2",
            "--json",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let doc: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("the same document on a pass");
    assert_eq!(doc["verdict"], "pass");
    assert_eq!(doc["criteria"].as_array().expect("criteria").len(), 2);
}

/// **§9.3.2 on the JUDGING verb: an account series is not evidence here either.**
///
/// ⚠ The first cut of `gate` excluded nothing and argued it owed nothing — "this verb selects by
/// an EXACT spec, and `parse` has already refused that spelling of `--require-kind`". That covers
/// the CRITERION side and not the EVIDENCE side. A spec is `VENUE:NAME`, two of a series' four
/// dimensions, so on a shared store `binance:BTCUSDT` reaches this account's `exec_fill` tape as
/// squarely as it reaches the bars: the presence criterion's `this spec holds:` cell named it and
/// the `--json` `series[]` carried its first_ts/last_ts/rows — out of a market-data read verb,
/// while `ls` over the same store showed neither and said so in a note.
///
/// The ANTI-VACUITY controls are the last two blocks: `ls` renders the SAME sentence over the SAME
/// store (so this is one rule and not a second one), and a gate over a store with no account
/// series carries no note at all (so the note is not simply always printed).
#[test]
fn the_gate_withholds_account_series_from_its_evidence_and_says_so() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_shared_account_store_datahub().to_string();

    // A kind the store does NOT hold, so the breach renders `this spec holds: …` — the one cell
    // that enumerates everything the spec matched, and the cell that used to leak.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--require-kind",
            "trade",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ Asserted on the ROW, not on the page. The DISCLOSURE names `exec_fill` deliberately — a
    // count alone cannot be acted on — so a page-wide `!contains` would be asserting the opposite
    // of §9.3.2's own rule, and it would fail on the note that proves the fix works.
    let holds = text
        .lines()
        .find(|l| l.contains("this spec holds:"))
        .unwrap_or_else(|| panic!("the presence criterion's own row: {text}"));
    assert!(holds.contains("bar"), "the MARKET series IS evidence: {holds}");
    assert!(!holds.contains("exec_fill"), "…and the account series is not: {holds}");

    let note = text
        .lines()
        .find(|l| l.starts_with("note: "))
        .unwrap_or_else(|| panic!("the withholding must be DISCLOSED: {text}"));
    assert!(note.contains("not part of this answer"), "{note}");
    assert!(note.contains("exec_fill"), "…naming the kind it withheld: {note}");
    assert!(note.contains("vike-cli account"), "…and the plane that will serve it: {note}");

    // The DOCUMENT carries neither the row nor the note. A consumer computes the difference from
    // `series_reported` and `series_matched`, which is this module's standing rule for `--json`.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--json",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let raw = stdout(&out);
    assert!(!raw.contains("exec_fill"), "no account row and no note in the document: {raw}");
    let doc: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {raw}"));
    assert_eq!(doc["series_reported"], 2, "the store's own total, account series included");
    assert_eq!(doc["series_matched"], 1, "…and what survived the exclusion");
    let series = doc["series"].as_array().expect("an array of series");
    assert_eq!(series.len(), 1, "{series:?}");
    assert_eq!(series[0]["kind"], "bar");

    // ONE rule, ONE sentence: `ls` over the same store withholds the same series in the same words.
    let listing = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr]);
    let text = stdout(&listing);
    assert!(text.contains("not part of this answer"), "{text}");
    assert!(text.contains("exec_fill"), "the note NAMES the kind it withheld: {text}");

    // ...and a store with no account series at all draws no note, so the assertions above cannot
    // be passing on a note that fires unconditionally.
    let clean = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1", "--addr", &clean],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!stdout(&out).contains("not part of this answer"), "{}", stdout(&out));
}

/// **THE GROUP LAYER, now that EVERY group answers.**
///
/// ⚠ **This was `every_unbuilt_group_is_refused_with_one_sentence_on_the_usage_rung`, and its FIRST
/// claim has died with the last unbuilt group.** It once compared three groups' refusals against
/// each other — three code paths, one sentence, because a reader who typed two of them and got two
/// wordings would learn that one was a different KIND of no. `catalog` and `source` shipped, leaving
/// `realtime` as the one group still refused at the `parse` rung; `realtime` ships here, so no
/// group is refused that way any more and asserting that one is would be a test
/// of a path that no longer runs. `crates/vike-cli/src/cmd/data.rs`'s `unbuilt_group_message` went
/// with it, for the reason its own doc had predicted: a function no caller reaches is
/// `-D dead-code`.
///
/// ⚠ **A SECOND claim died with it, and asserting it is what this round removed.** The surviving
/// loop still read `assert!(!err.contains("designed but not built yet"))` over all four groups, and
/// its doc called that the regression guard. It was not one: `unbuilt_group_message` is deleted and
/// the only two producers of that string left in the binary
/// (`crates/vike-cli/src/cmd/data.rs`'s `--source` and `--format` refusals) need a FLAG, which none
/// of these four lines passes — so no code path this loop can reach could produce the needle, and a
/// future PR re-stubbing a group with any other wording would have passed it in silence. What holds
/// the claim instead is stated positively and CAN fail: a group that answers renders ITS OWN verb
/// roster under ITS OWN command label.
///
/// Two claims, each able to go red alone:
///
/// 1. a group that ANSWERS answers as ITSELF — its own diagnostic label, and its own verb roster
///    RENDERED into the refusal. A stub names no verbs and carries the plane's label, and a group
///    routed into a SIBLING's parser carries the sibling's;
/// 2. a group nobody has heard of is a DIFFERENT answer, so claim 1 cannot be passing because
///    everything errors alike.
#[test]
fn a_group_that_answers_never_reads_as_designed_but_unbuilt() {
    let scratch = tempfile::tempdir().expect("tempdir");

    // 1. THE REGRESSION GUARD. `data <group>` with no verb is the narrowest line that reaches each
    // group's own entry point, which is the exact call a stub used to serve.
    //
    // ⚠ The LABEL column is `data` for `hist` alone, and that asymmetry is real rather than an
    // oversight: `crate::cmd::data`'s own `parse` IS the `hist` parser (`run` routes the other
    // three to their modules above it), so a `hist` diagnostic is the plane's by construction.
    // The SENTINEL verb is a literal for the reason the roster above this test is one — these
    // rosters are private consts in three modules a test process cannot name. ⚠ It is not by
    // itself a proof of WHICH group answered: `source`'s two verbs are both `catalog`'s as well,
    // so there is no unique word to pick there. The LABEL is what carries that half, on all four.
    for (built, label, sentinel) in [
        ("hist", "data", "fetch"),
        ("catalog", "data catalog", "refresh"),
        ("realtime", "data realtime", "watch"),
        ("source", "data source", "ls"),
    ] {
        let out = run(scratch.path(), &["data", built]);
        let err = stderr(&out);
        // The DIAGNOSTIC line rather than the first line of stderr: `exit_for_parse_error` prints
        // `vike-cli <command>: <msg>` and then the usage page under it, and a startup warning ahead
        // of either is a thing this binary is allowed to emit. ⚠ The SPACE in the needle is what
        // tells the two apart — a startup line is `vike-cli:` (`crates/vike-cli/src/lib.rs`'s
        // `settings_warning_lines`), a command diagnostic is `vike-cli <command>:`.
        let first = err
            .lines()
            .find(|l| l.starts_with("vike-cli "))
            .unwrap_or_else(|| panic!("`data {built}` must print a diagnostic: {err}"));
        assert!(
            first.starts_with(&format!("vike-cli {label}:")),
            "`data {built}` must answer under its own label `{label}`: {err}"
        );
        // The roster the refusal RENDERS, read back out of the sentence — the same derivation the
        // bare-`data` case above uses, and the half a stub could not produce at all.
        let roster = first
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(inner, _)| inner.split('|').map(str::trim).collect::<Vec<_>>())
            .unwrap_or_else(|| panic!("`data {built}` must RENDER its verb roster: {first}"));
        assert!(
            roster.contains(&sentinel),
            "`data {built}`'s roster must name its own verb `{sentinel}`: {first}"
        );
        assert!(
            !roster.contains(&built),
            "that is the GROUP roster rather than `{built}`'s verbs — this line was answered one \
             rung too high: {first}"
        );
        assert_eq!(out.status.code(), Some(2), "a group with no verb is the USAGE rung: {err}");
    }

    // 2. An unknown group is a DIFFERENT answer, so the loop above cannot be passing by everything
    // erroring identically.
    let out = run(scratch.path(), &["data", "nosuchgroup"]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("unknown"), "an unknown group is not a designed one: {err}");
    assert!(!err.contains("designed but not built"), "{err}");

    // ⚠ This block asserted that `data realtime record ls` was refused as "designed and not built
    // (§11.1)". The verb group SHIPPED on 2026-09-22, so the assertion is REPLACED rather than
    // deleted: what has to hold now is that the word reaches its own group and gets that group's
    // own answer — here, the store refusal, because this scratch directory holds no settings
    // database. A `record` that had fallen back to the parent parser would answer `unknown verb`
    // on the USAGE rung instead, which is the regression this keeps watching for.
    let out = run(scratch.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "it reaches the STORE, not the parser: {}",
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(err.contains("vike-cli data realtime record:"), "under its own label: {err}");
    assert!(!err.contains("designed and not built"), "the group is BUILT: {err}");

    // ...and the DESIGNED-and-unbuilt half that remains is its REMOTE route, refused by name.
    let out = run(scratch.path(), &["data", "realtime", "record", "ls", "--addr", "1.2.3.4:9"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("designed and not built"), "it must not read as a typo: {err}");
    assert!(err.contains("0081"), "…and must point at the argument: {err}");
}

/// The venue feed [`spawn_md_datahub`] streams from: a `DataClient` that answers `subscribe_trades`
/// by pushing prints into the hub's sink on a thread of its own.
///
/// ⚠ **It is a producer rather than a recorder**, unlike `crates/vike-datahub/tests/md_hub.rs`'s
/// `ScriptedFeed`, and that is what this file needs: the property under test here is that a print
/// crosses the whole path — sink → publish tick → mailbox → socket → `read_frame` → a rendered row
/// on stdout — which no double that only records its calls can exercise.
///
/// The push is BOUNDED (a fixed number of prints on a 20 ms cadence) so the thread ends on its own:
/// a test binary that leaves an endless producer running pays for it in every later case.
struct PushFeed {
    venue: String,
    sink: Arc<dyn LiveDataSink>,
}

impl DataClient for PushFeed {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this fixture serves no bars"))
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this fixture serves no quotes"))
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let sink = Arc::clone(&self.sink);
        let venue = self.venue.clone();
        let symbol = symbol.to_string();
        thread::spawn(move || {
            for i in 0..300i64 {
                sink.trade(
                    &venue,
                    &symbol,
                    TradeTick {
                        ts: 1_700_000_000_000 + i,
                        local_ts: 0,
                        price: 100.0 + i as f64,
                        size: 0.5,
                        // Alternating, so a renderer that hard-coded one side would show it.
                        is_buyer_maker: i % 2 == 0,
                        // ⚠ EMPTY, as the hub itself leaves it — `MdFrame::Trades`' own doc: the
                        // envelope's symbol is authoritative and the per-tick one is blanked on the
                        // way in. A fixture that filled it would hide the mis-key the renderer is
                        // written to avoid.
                        symbol: String::new(),
                    },
                );
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(SubscriptionId(1))
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this fixture serves no book"))
    }
    fn unsubscribe(&mut self, _id: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// A datahub with the MARKET-DATA plane MOUNTED over [`PushFeed`], advertising `binance` alone.
///
/// ⚠ The venue and the lane are not arbitrary: `vike_model::venue_caps`' binance row declares
/// `trades: true`, and the hub refuses a lane a venue's declared caps do not serve through
/// `vike_data::require_live_verb` — so a fixture naming any other pair would be refused by the
/// server before a frame could prove anything.
fn spawn_md_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let hub = MdHub::new(
        Box::new(|venue: &str, sink: Arc<dyn LiveDataSink>| {
            Ok(Box::new(PushFeed { venue: venue.to_string(), sink }) as Box<dyn DataClient + Send>)
        }),
        vec!["binance".to_string()],
    );
    hub.spawn();
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, None, Some(hub), None, None);
    });
    addr
}

/// Every line of a `--format jsonl` stdout, as documents — and the assertion that each one IS one.
fn jsonl_rows(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("every jsonl line is one document ({e}): {l}"))
        })
        .collect()
}

/// **A PRINT CROSSES THE WHOLE PATH**, and the stream stops where it was told to.
///
/// End to end: a venue feed's `TradeTick` → the hub's sink → the publish tick → this subscriber's
/// mailbox → the socket → `read_frame` → a row on stdout. Nothing below is provable from a parser
/// test, and the `--events` bound is what makes a live stream assertable at all.
///
/// ⚠ `--for` rides beside `--events` as a FAILSAFE rather than as the property under test: without
/// it a broken path would park in the read deadline the server's own heartbeat period armed (45s at
/// the floor) before failing. With it, a stream that delivers nothing ends on the time bound —
/// still exit 0, since the bound WAS reached — and the row assertions below are what redden.
#[test]
fn watch_streams_frames_to_stdout_and_stops_at_the_events_bound() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_md_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--events",
            "2",
            "--for",
            "20s",
            "--format",
            "jsonl",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "the bound was reached: {}", stderr(&out));
    let text = stdout(&out);
    let rows = jsonl_rows(&text);
    let trades: Vec<&serde_json::Value> = rows.iter().filter(|r| r["type"] == "trades").collect();
    assert!(
        trades.len() >= 2,
        "the --events bound is TWO data frames, and every one of them must be on stdout: {text}"
    );
    let first = trades[0];
    assert_eq!(first["venue"], "binance");
    // ⚠ The ENVELOPE's symbol, on a fixture whose ticks carry an empty one. A renderer that
    // serialized `TradeTick` would put `""` here and anything grouping by it would fold every
    // venue's tape into one bucket.
    assert_eq!(first["symbol"], "BTCUSDT", "{first}");
    let prints = first["prints"].as_array().expect("a trades frame carries its prints");
    assert!(!prints.is_empty(), "{first}");
    assert!(prints[0]["price"].is_number(), "{first}");
    assert!(prints[0].get("symbol").is_none(), "no empty per-tick symbol rides as an answer");

    // **STDOUT CARRIES FRAMES AND NOTHING ELSE.** The subscription note and the closing summary are
    // this binary's own prose and belong on stderr, or a `| jq` chokes on them.
    assert!(!text.contains("watching binance"), "the subscription note is not a frame: {text}");
    assert!(!text.contains("stream ended"), "the summary is not a frame: {text}");
    let err = stderr(&out);
    assert!(err.contains("watching binance BTCUSDT"), "…and it is still SAID, on stderr: {err}");
    assert!(err.contains("stream ended"), "{err}");
    assert!(err.contains("data,"), "the summary counts by class: {err}");

    // ...and `--out FILE` moves exactly that stream off stdout, leaving it EMPTY — which is what
    // makes the file a tape rather than a transcript.
    //
    // ⚠ **Its OWN datahub, and that is not tidiness — reusing the first one is a TIMER.**
    // `PushFeed::subscribe_trades` pushes 300 ticks at 20 ms and ends, and it runs ONCE per venue
    // subscription: `MdHub` holds the key through `MD_LINGER` after the first session closes, so
    // re-acquiring it calls no second `subscribe_trades` and the second run rides the FIRST run's
    // producer. On a loaded box, a first run plus process teardown plus this spawn taking longer
    // than that ~6 s life leaves this watch subscribed to a finished producer: nothing arrives, the
    // `--for 20s` failsafe ends it at exit 0, and the row assertion below reddens for a reason that
    // has nothing to do with `--out`. A fresh hub is a fresh producer with its own clock.
    let out_addr = spawn_md_datahub().to_string();
    let tape = scratch.path().join("tape.jsonl");
    let tape_arg = tape.to_string_lossy().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--events",
            "1",
            "--for",
            "20s",
            "--out",
            &tape_arg,
            "--addr",
            &out_addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "with --out, stdout carries nothing at all");
    let written = std::fs::read_to_string(&tape).expect("the tape file");
    let file_rows = jsonl_rows(&written);
    assert!(
        file_rows.iter().any(|r| r["type"] == "trades"),
        "the frames went to the file: {written}"
    );
    // The default rendering follows the DESTINATION: a file is not a terminal, so it got the
    // machine form without being asked — which is what `jsonl_rows` above just proved by parsing it.
    assert!(
        !written.contains("trades binance BTCUSDT seq="),
        "a file must not get the human table form: {written}"
    );
}

/// A datahub that HANDSHAKES correctly and then MISBEHAVES: it answers `Hello` and `MdSubscribe`
/// exactly as the real server does — advertising the market-data plane and `binance`, and accepting
/// the spec verbatim — and then hands the socket to `after`, which is where the fault under test is
/// planted.
///
/// ⚠ Hand-rolled rather than [`spawn_md_datahub`], and the reason is the property: what this binary
/// does when the WIRE breaks cannot be driven from a server that works, and a real one does not
/// break on request. Everything up to the last frame is the real exchange, so the CLI reaches
/// `stream_frames` by the ordinary path.
fn spawn_scripted_md_server(after: fn(&mut TcpStream)) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("the CLI dials immediately");
        let _: Request = read_frame(&mut s).expect("the client opens with Hello");
        write_frame(
            &mut s,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec![FEATURE_MARKET_DATA.to_string(), md_venue_feature("binance")],
                nonce: None,
            },
        )
        .expect("the welcome");
        let specs = match read_frame::<_, Request>(&mut s).expect("the subscription") {
            Request::MdSubscribe { specs } => specs,
            _ => panic!("the CLI's next frame after the handshake is MdSubscribe"),
        };
        write_frame(
            &mut s,
            &Response::MdSubscribed {
                session: MdSessionId(1),
                // Accepted VERBATIM: nothing here is testing a clamp or a refusal.
                accepted: specs,
                refused: Vec::new(),
                heartbeat_ms: 1_000,
            },
        )
        .expect("the acceptance");
        after(&mut s);
    });
    addr
}

/// **A BROKEN STREAM IS NOT A FINISHED ONE, and `--unbounded` does not change that** — end to end,
/// in the one channel a wrapper reads.
///
/// ⚠ This is the case for a defect that was PINNED: `End::was_asked_for` folded every non-bound end
/// together under `--unbounded`, so a transport fault and a protocol desync exited 0, identically
/// to a clean stop. A wrapper running `… --unbounded --out tape.jsonl` under `set -e` took a
/// half-written tape for a complete capture. The unit suite beside
/// `crates/vike-cli/src/cmd/data/realtime.rs`'s `End` drives the classification over a real socket;
/// this is the half that proves it reaches the EXIT CODE.
#[test]
fn an_unbounded_watch_that_faults_exits_non_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let tape = scratch.path().join("broken.jsonl");
    let tape_arg = tape.to_string_lossy().to_string();

    // A TRANSPORT FAULT: a well-framed body that does not decode, which is how one reaches this
    // verb (`read_frame` fuses framing and decoding into one `InvalidData`).
    let broken = spawn_scripted_md_server(|s| {
        s.write_all(&3u32.to_be_bytes()).expect("a valid length prefix");
        s.write_all(b"{{{").expect("...and a body that is not JSON");
        s.flush().expect("the scripted bytes are on the wire");
    })
    .to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--unbounded",
            "--out",
            &tape_arg,
            "--addr",
            &broken,
        ],
    );
    let err = stderr(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a faulted stream may not exit 0 — a wrapper cannot tell it from a finished capture: {err}"
    );
    assert!(err.contains("the stream FAILED"), "…and it says which of the two it was: {err}");

    // THE CONTROL, and it is what stops the case above passing because this harness always fails:
    // the SAME line against a server that says GOODBYE — an ENDING rather than a break — is exit 0,
    // because `--unbounded` named no bound to miss.
    let polite = spawn_scripted_md_server(|s| {
        write_frame(s, &Response::Md(Box::new(MdFrame::Bye(MdBye::ServerStopping))))
            .expect("the goodbye");
    })
    .to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--unbounded",
            "--addr",
            &polite,
        ],
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "a goodbye ENDED the stream: {err}");
    assert!(err.contains("the server ended the stream"), "{err}");
}

/// A server with NO market-data plane is a RUN failure naming the capability, and nothing is
/// streamed.
///
/// ⚠ The rung is the run-failure one rather than connect-class: the socket was fine and the
/// capability was missing, which is a served answer — the same split
/// `crates/vike-cli/src/cmd/data.rs`'s `connect` already makes for the `hist` read verbs. And the
/// refusal is LOCAL: `DatahubClient::md_subscribe` checks the advertisement before sending, so the
/// message names the key an operator has to set on the SERVER.
#[test]
fn watch_against_a_server_with_no_market_data_plane_names_the_capability() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--events",
            "1",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(1), "a server that ANSWERED is the run-failure rung");
    let err = stderr(&out);
    assert!(err.contains("market_data"), "the refusal names the missing capability: {err}");
    assert_eq!(stdout(&out), "", "nothing was printed as though a frame had arrived");
}

/// **`status` REPORTS AN ADVERTISEMENT AND SAYS SO**, in both renderings and in both server shapes.
///
/// The pairing is the whole test: a datahub with the plane mounted lists the venue it advertises,
/// one without it says NOT advertised — and neither is allowed to read as a health check, because
/// nothing here observed a frame. §11's word for this verb is "health", which is exactly why the
/// output has to contradict that expectation in its own words.
#[test]
fn status_reports_an_advertisement_and_never_a_liveness_verdict() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let serving = spawn_md_datahub().to_string();
    let bare = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "realtime", "status", "--addr", &serving]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ **THE TABLE IS A TABLE**, asserted before anything is read out of it. A child's stdout is
    // never a terminal, so a default that followed the destination answered here in JSON — and
    // every assertion below passed on that document's own keys (`market_data_advertised` contains
    // `advertised`; the venue is a string in it). This line is what makes the rest mean what they
    // say; `default_render`'s doc carries the incident.
    assert!(!text.trim_start().starts_with('{'), "the default answer is a table, not JSON: {text}");
    assert!(
        text.lines().any(|l| l.starts_with("binance")),
        "the advertised venue is a ROW of its own: {text}"
    );
    assert!(text.contains("LIVE FEED"), "…under the column that says what it is: {text}");
    assert!(text.contains("not a liveness check"), "{text}");
    // The claim it may never make. `healthy` is what a reader expects from the word `status`, and
    // printing it about a venue nothing probed is positive confirmation of something false.
    assert!(!text.to_lowercase().contains("healthy"), "nothing here measured health: {text}");

    let doc: serde_json::Value = serde_json::from_str(&stdout(&run(
        scratch.path(),
        &["data", "realtime", "status", "--json", "--addr", &serving],
    )))
    .expect("stdout is one JSON document");
    assert_eq!(doc["market_data_advertised"], true);
    assert_eq!(doc["venues"][0], "binance");
    assert_eq!(doc["count"], 1);
    assert_eq!(doc["liveness_probed"], false, "the field a consumer must not assume away: {doc}");

    // The OTHER shape — and it is what stops the assertions above passing on a verb that says
    // `advertised` whatever it was told.
    let out = run(scratch.path(), &["data", "realtime", "status", "--addr", &bare]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.trim_start().starts_with('{'), "a table here too: {text}");
    assert!(text.contains("NOT advertised"), "{text}");
    assert!(text.contains("market_data"), "it names the capability that is missing: {text}");
    assert!(text.contains("no venue advertises"), "{text}");
    assert!(text.contains("not a liveness check"), "every answer ends on it: {text}");
}

/// Every `data realtime` line an operator gets wrong is caught BEFORE a socket is opened — which is
/// what the USAGE rung means here, and what keeps a wrong command line costing nothing.
///
/// ⚠ None of these names an `--addr`, so a case that reached the dial would try the default
/// 127.0.0.1:7878 — a real datahub on a deployed box. Exit 2 is the proof that none of them does:
/// `parse` runs before `connect`, and a connect-class failure is rung 3.
#[test]
fn a_bad_realtime_command_line_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data", "realtime", "watch", "binance:BTCUSDT", "--lane", "trades"], "BOUNDED"),
        (vec!["data", "realtime", "watch", "binance:BTCUSDT", "--events", "1"], "needs --lane"),
        (
            vec!["data", "realtime", "watch", "b:S", "--lane", "quotes", "--events", "1"],
            "no quotes lane",
        ),
        (
            vec![
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT:1h",
                "--lane",
                "depth",
                "--events",
                "1",
            ],
            "INTERVAL",
        ),
        (
            vec![
                "data", "realtime", "watch", "b:S", "--lane", "trades", "--depth", "5", "--events",
                "1",
            ],
            "--depth does not apply",
        ),
        (
            vec!["data", "realtime", "watch", "b:S", "--lane", "trades", "--events", "1", "--json"],
            "jsonl",
        ),
        (vec!["data", "realtime", "watch", "b:S", "--lane", "trades", "--for", "3d"], "record"),
        (
            vec![
                "data",
                "realtime",
                "watch",
                "b:S",
                "--lane",
                "trades",
                "--unbounded",
                "--events",
                "1",
            ],
            "contradicts",
        ),
        (vec!["data", "realtime", "status", "--lane", "trades"], "does not apply to `status`"),
        (vec!["data", "realtime", "status", "--format", "jsonl"], "ONE question about ONE server"),
        (vec!["data", "realtime", "frobnicate"], "unknown `data realtime` verb"),
    ] {
        let out = run(scratch.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?} is a usage error: {out:?}");
        let err = stderr(&out);
        assert!(err.contains(needle), "{args:?} must say {needle:?}: {err}");
    }
}

/// The group's help is the only place its verbs are named — it takes no default action — and a
/// `--help` that exited non-zero would break every `set -e` caller.
///
/// ⚠ The verb check reads the LABEL COLUMN rather than the page, because every verb name also
/// occurs in the surrounding prose: a `contains` would pass with a verb's whole block deleted, which
/// is the measured mistake `crates/vike-cli/src/cmd/data/catalog.rs`'s usage test records.
#[test]
fn realtime_help_names_both_verbs_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "realtime", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ `record` joined this list on 2026-09-22 and is a SUB-GROUP rather than a verb — it is
    // checked here all the same, because this page is the only place it is reachable from.
    for verb in ["watch", "status", "record"] {
        assert!(
            text.lines().any(|l| l.starts_with(&format!("  {verb}"))),
            "`{verb}` has no block of its own on the page: {text}"
        );
    }
    assert!(text.contains("STDOUT CARRIES FRAMES"), "the rule a pipeline depends on: {text}");
    // ...and the group is reachable from the PLANE's own page, or nobody finds it.
    let plane = stdout(&run(scratch.path(), &["data", "--help"]));
    assert!(plane.contains("realtime"), "{plane}");
    assert!(
        !plane.contains("designed and not built"),
        "the plane page may not still advertise this group's verbs as unbuilt: {plane}"
    );
}

/// A datahub that REQUIRES authentication — it holds node keys, so its `Welcome` advertises
/// `auth` and every connection must complete the mac handshake before a verb is answered.
///
/// A fixture of its own, for [`spawn_shared_account_store_datahub`]'s stated reason and one more:
/// this is the only server here that REFUSES a caller, and the caller under test is a `vike-cli`
/// run holding no keys at all. The store is empty on purpose — nothing in this fixture's cases
/// reaches a verb, which is the whole point of it.
///
/// ⚠ This opened "A FOURTH fixture", and the NUMBER was the defect rather than the sentence: a
/// hand-maintained count in prose that nothing holds in step, so a fixture landing above it makes
/// this doc silently wrong with no test able to notice — the same rot
/// `crates/vike-cli/tests/message_literals.rs`'s `source_files` doc had to correct in its own
/// measurement. The distinguishing property is what carries the argument, and unlike an ordinal it
/// is a property rather than a position.
fn spawn_keyed_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let keys = NodeKeys::new(b"an-observe-key".to_vec(), b"a-control-key".to_vec());
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, Some(keys), None, None, None);
    });
    addr
}

/// **A SERVED REFUSAL IS NOT AN ABSENT DATAHUB — through the CALL SITE, against a real server.**
///
/// ⚠ **This is the case that was missing, and the gap had a precise shape.** The refused path was
/// covered by two unit tests that between them never met each other: one over the CLASSIFIER
/// (`answered_and_refused`, a table of `io::ErrorKind`s) and one over the RENDERER (a hand-built
/// `ServerView::Refused`). Nothing ran `ask_the_server`, so inverting its guard to
/// `Err(e) if !answered_and_refused(e.kind())` — or reverting it to fold every failure into
/// `connect` again — left both of them green while a PROTO_VERSION skew rendered as `NOT REACHED`,
/// the exact defect the third `ServerView` variant was added for.
///
/// The server here refuses for the commonest reason in production: it is KEYED and this run holds
/// no node keys at all, so `DatahubClient::connect` completes the handshake and then raises
/// `PermissionDenied`. The CLI must still answer 0 with its whole local matrix.
///
/// ⚠ **"holds no node keys" is a property of the CHILD's ENVIRONMENT, not of the temp directory** —
/// [`run`] is where it is made true, and its doc carries why: `datahub_keyring` reads the
/// environment BEFORE the store, so on a box exporting the pair this run took `connect_authed` and
/// the refusal arrived as a denied mac. Same verdict, different path from the one named above.
#[test]
fn the_capability_matrix_says_which_side_refused_when_a_server_answers_and_says_no() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_keyed_datahub().to_string();

    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr]);
    assert!(out.status.success(), "a refusal is REPORTED, never fatal: {}", stderr(&out));
    let body = stdout(&out);
    assert!(body.contains("REACHED, and it REFUSED"), "{body}");
    assert!(
        !body.contains("NOT REACHED"),
        "a server that answered the handshake may not be rendered as an absent one: {body}"
    );
    // The local half is untouched — the verb still answers whoever is on that port.
    for venue in vike_model::VENUES {
        assert!(body.contains(venue), "every roster venue must be rendered: {body}");
    }
    // ⚠ The needle is the string the ANSWERED arm's capability loop renders for a `None` — see the
    // unit twin `a_server_that_answered_and_refused_is_not_rendered_as_an_absent_one`. This
    // asserted `not served`, which `venues_lines` renders in no arm at all, so it could not fail:
    // the mutation it is here for (moving that loop out of the answered arm) prints
    // `was not asked for`, the very claim about the far side this branch deleted.
    assert!(
        !body.contains("was not asked for"),
        "nothing was read here, so no capability row may be rendered — least of all as unasked: \
         {body}"
    );
    assert_eq!(body.matches(&addr).count(), 1, "the address is named once on that line: {body}");

    // …and the DOCUMENT carries the three-state token rather than a boolean that would have to
    // call this server unreachable.
    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["server"]["state"], "refused");
    assert!(doc["server"]["features"].is_null(), "a discarded handshake advertised nothing: {doc}");

    // THE ANTI-VACUITY CONTROL: a KEYLESS server on the same code path is `answered`, so the
    // assertions above are about this server's refusal and not about every server.
    let open = spawn_seeded_datahub().to_string();
    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &open, "--json"]);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout is one JSON document");
    assert_eq!(doc["server"]["state"], "answered", "{doc}");
}

/// A datahub over a store holding ONLY this account's own activity — a fill and no bars.
///
/// ⚠ Its own fixture rather than a flag on [`spawn_shared_account_store_datahub`], and the reason
/// is the case below: the combination under test is "the spec matched something, and everything it
/// matched was withheld", which needs a store with NO market series at all. It is the only store
/// here that has none — every other fixture in this file seeds bars. (This opened "A FIFTH
/// fixture"; see [`spawn_keyed_datahub`] for why the ordinal went and the property stayed.)
fn spawn_account_only_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = MemHistStore::new();
    store.append_exec_fills("binance", "BTCUSDT", &[fill(DAY_MS)], None).expect("seed a fill");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// **THE RUNG THE EXCLUSION MOVES.** A spec that matches account series and NOTHING ELSE is the
/// nothing-was-evaluated rung (`7`), and the refusal says what it withheld — because the operator
/// can see those series with their own eyes and "matches none of them" alone is a refusal they can
/// disprove in one command.
///
/// ⚠ **This combination was reached by no test at any level, and it is the one place the exclusion
/// changes an EXIT CODE.** Before the exclusion this store answered `6` (a breach: the series was
/// found and failed the day threshold); it answers `7` now. The withheld note is stitched into
/// `CliError::empty`'s message at exactly one site, and every other case in this file seeds bars
/// beside the fill — so dropping that stitching, or losing the `\n` that puts the note on its own
/// line, changed nothing any test could see.
///
/// The ANTI-VACUITY control is the last block: the SAME spec against a store with market data
/// answers a different rung entirely, so this case is about the withholding rather than about the
/// spec being unmatchable.
#[test]
fn a_spec_matching_only_withheld_account_series_is_the_empty_rung_and_names_what_it_withheld() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_account_only_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "hist", "gate", "binance:BTCUSDT", "--require-days", "1", "--addr", &addr],
    );
    assert_eq!(
        out.status.code(),
        Some(7),
        "everything this spec matched was withheld, so NOTHING was evaluated — and that is not a \
         pass: {}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "", "no criteria were judged, so there is no verdict to print");

    let err = stderr(&out);
    assert!(err.contains("nothing to gate"), "{err}");
    // THE PROPERTY: the note is a LINE of its own, not a clause welded onto the end of the
    // sentence above it. That is what the `\n` in the stitching buys, and it is invisible to a
    // `contains` over the whole message.
    let note = err
        .lines()
        .find(|l| l.starts_with("note: "))
        .unwrap_or_else(|| panic!("the withholding must be DISCLOSED, on its own line: {err}"));
    assert!(note.contains("not part of this answer"), "{note}");
    assert!(note.contains("exec_fill"), "…naming the kind it withheld: {note}");
    assert!(note.contains("vike-cli account"), "…and the plane that will serve it: {note}");

    // The ANTI-VACUITY control: the same spec over a store that DOES hold market data reaches the
    // judging half instead, so the rung above is the withholding and not an unmatchable spec.
    let shared = spawn_shared_account_store_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "hist", "gate", "binance:BTCUSDT", "--require-days", "1", "--addr", &shared],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("PASS"), "{}", stdout(&out));
}

/// **THE WALK, ON THE SHIPPED BINARY** — `export --addr --format jsonl` streams a remote store's
/// rows into `--out`, and the file it writes is the same whether the range took one request or
/// three.
///
/// ⚠ **That last clause is the whole test.** Every other property here (the file exists, it holds
/// rows, the rows are JSON) would pass a one-shot implementation that never walked at all. What
/// only a correct walk gives is INVARIANCE UNDER THE STEP: `Request::LoadBars`' bounds are
/// INCLUSIVE on both ends, so a window that began where the previous one ended would duplicate
/// every row landing on a boundary.
///
/// ⚠⚠ **`--from 1` RATHER THAN `--from 0`, AND THE REASON IS A MEASUREMENT — THIS CASE FAILED TO
/// FAIL ONCE ALREADY.** The first version walked `[0, 2d]` in `1d` steps and its comment claimed a
/// boundary row was "EVERY row". It is none of them: a window ends at `lo + step - 1`, so with
/// `--from 0` the ends fall on `1d-1` and `2d-2`, where this fixture has no bar — and the
/// kill-proof (mutating `windows`' `lo = hi.saturating_add(1)` to `lo = hi` in
/// `crates/vike-cli/src/cmd/data/export.rs`) left this test GREEN while the unit case reddened.
/// Starting at `1` moves the first window's end to exactly `1d`, where a bar IS, so the mutation
/// re-fetches it and the two files differ. Re-verified: mutated → FAIL, reverted → PASS.
#[test]
fn a_remote_export_streams_rows_and_the_file_does_not_depend_on_the_step() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let walked = scratch.path().join("walked.jsonl");
    let one_shot = scratch.path().join("one-shot.jsonl");
    // ⚠ Fixed epoch bounds, so nothing depends on the wall clock — and `1` rather than `0` for the
    // boundary reason above, which costs the `ts=0` bar and buys the only assertion here that can
    // fail for its stated reason.
    let to = (2 * DAY_MS).to_string();

    let argv = |out: &Path, window: &str| -> Vec<String> {
        [
            "data",
            "hist",
            "export",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--format",
            "jsonl",
            "--from",
            "1",
            "--to",
            &to,
            "--window",
            window,
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain(["--out".to_string(), out.display().to_string()])
        .collect()
    };
    // A `fn` rather than a closure: a closure's inferred signature ties the borrow of `v` to the
    // borrow of the returned slice, which does not typecheck here.
    fn as_args(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    // TWO windows: `1d` over the `[1, 2d]` inclusive range tiles as [1,1d] [1d+1,2d] — and the
    // first one's END is exactly where a bar sits, which is the property the whole case turns on.
    let v = argv(&walked, "1d");
    let out = run(scratch.path(), &as_args(&v));
    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains("2 windows"), "the walk reports how many steps it took: {said}");
    assert!(said.contains("2 bar rows"), "…and how many rows landed: {said}");

    // ONE window: the same range in a single request.
    let v = argv(&one_shot, "30d");
    let out = run(scratch.path(), &as_args(&v));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("1 window)"), "{}", stdout(&out));

    let a = std::fs::read_to_string(&walked).expect("the walked file");
    let b = std::fs::read_to_string(&one_shot).expect("the one-shot file");
    assert_eq!(a, b, "the step may not change the ROWS — an inclusive-bound off-by-one duplicates");

    // ...and the rows are the two bars inside that range, ascending, self-describing. The `ts=0`
    // bar is OUTSIDE it by one millisecond, which is the cost of the boundary alignment above.
    let rows: Vec<serde_json::Value> =
        a.lines().map(|l| serde_json::from_str(l).expect("one object per line")).collect();
    assert_eq!(rows.len(), 2, "{a}");
    assert_eq!(
        rows.iter().map(|r| r["ts"].as_i64().expect("ts")).collect::<Vec<_>>(),
        vec![DAY_MS, 2 * DAY_MS]
    );
    for r in &rows {
        assert_eq!(r["venue"], "binance", "every row carries its series: {r}");
        assert_eq!(r["interval"], "1h");
        // ANTI-VACUITY for the equality above: the rows are not empty objects, and the OPTIONAL
        // columns are ABSENT rather than null — the shape `get --format jsonl` emits.
        assert_eq!(r["close"], 1.5);
        assert!(r.get("bid").is_none(), "an unrecorded bid is an ABSENT key, never null: {r}");
    }
}

/// The CSV half, and the three decisions it had to make — a header that always lands, minimal
/// quoting, an EMPTY field for a missing value — proven on the shipped binary rather than in a unit.
#[test]
fn a_remote_csv_export_writes_a_header_and_one_line_per_row() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out_path = scratch.path().join("btc.csv");
    let to = (2 * DAY_MS).to_string();
    let path = out_path.display().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "export",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--format",
            "csv",
            "--from",
            "0",
            "--to",
            &to,
            "--out",
            &path,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let text = std::fs::read_to_string(&out_path).expect("the csv file");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "one header plus three rows: {text}");
    assert!(lines[0].starts_with("venue,symbol,interval,ts,ts_utc,open,"), "{}", lines[0]);
    // THE WIDTH INVARIANT: a row that lost a column would shift every field to its right, which is
    // the one CSV failure a reader cannot detect.
    let width = lines[0].split(',').count();
    for l in &lines[1..] {
        assert_eq!(l.split(',').count(), width, "row width must match the header: {l}");
    }
    // The NULL SPELLING: this fixture's bars carry no bid/ask/funding, so those three columns are
    // EMPTY rather than the word `null` — which would parse as text and poison the column.
    assert!(lines[1].ends_with(",,,"), "absent values are empty fields: {}", lines[1]);
    // ANTI-VACUITY: the same row's PRESENT values are not empty, so the check above is about
    // absence rather than about every field being blank.
    assert!(lines[1].starts_with("binance,BTCUSDT,1h,0,"), "{}", lines[1]);
}

/// The two refusals this route carries that a reader is likeliest to meet, on the shipped binary:
/// `--format parquet` over `--addr`, and a bulk range with only one bound.
///
/// ⚠ Each must name a COMMAND LINE that works rather than a phase or a diagnosis — the rule the
/// rest of this plane's refusals already follow.
#[test]
fn the_remote_export_refusals_name_a_command_that_works() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let path = scratch.path().join("x").display().to_string();
    let base = ["data", "hist", "export", "binance:BTCUSDT:1h", "--addr", &addr, "--out", &path];

    let mut argv = base.to_vec();
    argv.extend(["--format", "parquet", "--from", "0", "--to", "1"]);
    let out = run(scratch.path(), &argv);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("drop --addr"), "the route that DOES write parquet: {err}");
    assert!(err.contains("backend-agnostic"), "…and the measurement, not a phase: {err}");
    assert!(!err.contains("not built yet"), "…which is not the same as unbuilt: {err}");

    let mut argv = base.to_vec();
    argv.extend(["--format", "jsonl", "--from", "0"]);
    let out = run(scratch.path(), &argv);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("BOTH --from and --to"), "{err}");
    assert!(
        err.contains("data hist ls --venue binance --name BTCUSDT"),
        "…and where to read the \
                                                                         span: {err}"
    );

    // ANTI-VACUITY for both: the same line with the missing piece supplied SUCCEEDS, so each
    // refusal is about what it names rather than about the fixture being unreachable.
    let mut argv = base.to_vec();
    argv.extend(["--format", "jsonl", "--from", "0", "--to", "1"]);
    assert!(run(scratch.path(), &argv).status.success());
}

// ─── `data realtime record` — the subscription ROWS this box persists ────────────────────────────

/// The recorder profile every `record` case starts from: one FAMILY row and one SYMBOLS row, which
/// is the shape the CI box actually records.
///
/// ⚠ It has to ROUND-TRIP through `vike_secrets::profile_store::render_recorder_toml`, because
/// `vike-cli config mirror --recorder` refuses to store a body that does not reproduce the file it
/// came from — so an extra key here fails the FIXTURE rather than the case.
const RECORDER_PROFILE: &str = concat!(
    "store = \"market_data/hist\"\n\n",
    "[[subscribe]]\nvenue = \"polymarket\"\nfamily = \"btc-updown-5m\"\nbackfill = \"off\"\n\n",
    "[[subscribe]]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT.P\"]\n",
);

/// A settings directory with a MIGRATED store and nothing else in it.
///
/// ⚠ **`datahub_addr` is pinned at a port nothing serves, and that is load-bearing rather than
/// tidy.** `record add` dials the configured datahub to ask which venues it can RECORD, and the
/// default rung is `127.0.0.1:7878` — the address a developer box or a CI lane may genuinely have
/// a datahub on. Pinning it here makes every case take the same documented branch (unreachable →
/// WARN and write) instead of one that depends on what else is running on the box.
///
/// ⚠ ONE obviously-fake DEMO key, for `crates/vike-cli/tests/seal_enforcement.rs`'s reason:
/// `secrets migrate` refuses to create a database when there is nothing to move, so an empty file
/// leaves no store for `config mirror` to write into.
fn migrated_settings_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("secrets.env"), "BINANCE_DEMO_API_KEY=fixture-not-a-real-key\n")
        .expect("write secrets.env");
    std::fs::write(dir.path().join("config.toml"), "datahub_addr = \"127.0.0.1:1\"\n")
        .expect("write config.toml");
    let out = run(dir.path(), &["secrets", "migrate"]);
    assert!(out.status.success(), "the fixture's migrate must succeed: {}", stderr(&out));
    dir
}

/// …and with one recorder profile in it, mirrored through the SHIPPED verb, so the fixture is a
/// state an operator can actually produce.
fn recorder_settings_dir(name: &str, active: bool) -> tempfile::TempDir {
    let dir = migrated_settings_dir();
    let profile = dir.path().join("rec.toml");
    std::fs::write(&profile, RECORDER_PROFILE).expect("write the profile");
    let out = run(
        dir.path(),
        &[
            "config",
            "mirror",
            "--recorder",
            profile.to_str().expect("utf-8 temp path"),
            "--recorder-name",
            name,
        ],
    );
    assert!(out.status.success(), "the fixture's mirror must succeed: {}", stderr(&out));
    if active {
        // ⚠ Through the LIBRARY, because no shipped verb selects a recorder profile yet — the
        // design's ruling 5 names that verb as work of its own, and `config mirror` deliberately
        // withholds the active row. A test is outside
        // `crates/vike-ops/tests/profile_writer_gate.rs`'s scope by that gate's own statement, so
        // this is not a second production writer.
        vike_secrets::profile_store::set_active(
            &vike_secrets::db_path_in(dir.path()),
            vike_secrets::profile_store::ProfileKind::Recorder,
            name,
            &vike_secrets::profile_store::OperatorWrite::claim("data_cli.rs fixture"),
            0,
            AssetClass::SQL_WORDS,
        )
        .expect("the fixture's active row must be settable");
    }
    dir
}

/// The group's help is the only place its verbs are named — it takes no default action — and a
/// `--help` that exited non-zero would break every `set -e` caller.
///
/// ⚠ The verb check reads the LABEL COLUMN rather than the page, for the reason
/// [`realtime_help_names_both_verbs_and_exits_zero`] states: every verb name also occurs in the
/// surrounding prose, so a `contains` would pass with a verb's whole block deleted.
#[test]
fn record_help_names_every_verb_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "realtime", "record", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for verb in ["ls", "add", "rm"] {
        assert!(
            text.lines().any(|l| l.starts_with(&format!("  {verb}"))),
            "`{verb}` has no block of its own on the page: {text}"
        );
    }
    assert!(text.contains("NEXT RESTART"), "the ruling an operator must carry: {text}");
    // ...and the sub-group is reachable from its PARENT's page, or nobody finds it.
    let parent = stdout(&run(scratch.path(), &["data", "realtime", "--help"]));
    assert!(parent.lines().any(|l| l.starts_with("  record")), "{parent}");
}

/// **The three noes of ruling 5 are three DIFFERENT answers**, and an UNMIGRATED box gets the one
/// that names the verb which creates a store — not the one that names the verb which creates a
/// profile.
#[test]
fn record_on_a_box_with_no_store_names_the_verb_that_creates_one() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(out.status.code(), Some(1), "a missing store is a run failure: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("secrets migrate"), "{err}");
    assert!(!err.contains("NONE is marked active"), "a DIFFERENT no: {err}");

    // A MIGRATED box with no recorder profile is the SECOND no, and it names the other verb.
    let dir = migrated_settings_dir();
    let out = run(dir.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("config mirror --recorder"), "{err}");
    assert!(!err.contains("secrets migrate"), "the store EXISTS here: {err}");

    // A profile that is stored and NOT selected is the THIRD, and it names the flag that picks one.
    let dir = recorder_settings_dir("default", false);
    let out = run(dir.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("--profile NAME"), "{err}");
    assert!(err.contains("Recorder profiles in this store: default."), "{err}");
    // ...and naming it is the answer, which is what makes the refusal above actionable.
    let out = run(dir.path(), &["data", "realtime", "record", "ls", "--profile", "default"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("family btc-updown-5m"), "{}", stdout(&out));
}

/// **⚠ THE CROSS-KIND KILL PROOF, END TO END, on the CI box's own shape.**
///
/// `profile.name` is `TEXT PRIMARY KEY` — ONE namespace across all three kinds — and
/// `vike_secrets::profile_store::store_profile` replaces a body with `DELETE` + `INSERT` while
/// PRESERVING the `active` bit it finds. the CI box's store holds an ACTIVE `recorder` profile called
/// `default` (`deploy/vike-datahub.service` runs `--recorder-profile default`), so
/// `config mirror --no-settings --profile <run.toml> --profile-name default` is ONE flag away from
/// deleting that body and its `subscription` rows and re-inserting the name as `kind = 'run'` with
/// `active = 1` inherited — arming the live pre-trade ceiling plane with no
/// `config activate --proves` in front of it, while the report printed *"No active run row was
/// written … this box resolves its run profile exactly as it does today"*.
///
/// It lives in THIS file rather than beside the other `config` cases because
/// [`recorder_settings_dir`] is the fixture that produces exactly that shape through the shipped
/// verbs, and a second copy of it next door is a fixture that can drift from this one.
///
/// ⚠ `--dry-run` is asserted FIRST and deliberately: `config mirror` plans every half before it
/// writes any of them, so a rehearsal that answered *would mirror* here would be promising a write
/// the store was going to refuse — the finding's own "positive confirmation of something false",
/// one layer up.
#[test]
fn a_mirror_onto_a_name_another_kind_holds_is_refused_and_destroys_nothing() {
    let dir = recorder_settings_dir("default", true);
    let run_profile = dir.path().join("run-live.toml");
    std::fs::write(
        &run_profile,
        "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 100.0\nmax_total_exposure = 500.0\n",
    )
    .expect("write the run profile");
    let argv = |extra: &'static str| -> Vec<String> {
        let mut v: Vec<String> = ["config", "mirror", "--no-settings", "--profile"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        v.push(run_profile.to_str().expect("utf-8 temp path").to_string());
        v.push("--profile-name".to_string());
        v.push("default".to_string());
        if !extra.is_empty() {
            v.push(extra.to_string());
        }
        v
    };

    for extra in ["--dry-run", ""] {
        let owned = argv(extra);
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let out = run(dir.path(), &args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "the cross-kind mirror must FAIL (extra: {extra:?}): {}{}",
            stdout(&out),
            stderr(&out)
        );
        let err = stderr(&out);
        for needle in ["recorder", "default", "NOTHING WAS WRITTEN"] {
            assert!(err.contains(needle), "the refusal must name {needle:?}: {err}");
        }
        assert!(
            !stdout(&out).contains("would mirror"),
            "a rehearsal may not promise a write the store refuses: {}",
            stdout(&out)
        );
    }

    // NOTHING was destroyed: the recorder body, its subscriptions and its `active` bit are all
    // still there, read back through the shipped verb that reads them.
    let out = run(dir.path(), &["data", "realtime", "record", "ls"]);
    assert!(out.status.success(), "the recorder profile must still resolve: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("family btc-updown-5m"), "the subscriptions survived: {text}");
    assert!(text.contains("polymarket") && text.contains("binance"), "both of them: {text}");
    assert!(
        text.contains("profile: default (active)"),
        "…and the `active` bit is still the RECORDER's: {text}"
    );

    // …and the RUN plane is still empty, which is the half an ArmingOutcome could never show.
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("the store must still read");
    assert_eq!(
        profiles.active(vike_secrets::profile_store::ProfileKind::Run),
        None,
        "THE ONE THAT MATTERS: no `run` row was armed by a verb that only stores bodies"
    );
    assert_eq!(
        profiles.all().len(),
        1,
        "and no second body landed beside the recorder: {:?}",
        profiles.all().iter().map(|p| (&p.row.name, p.row.kind)).collect::<Vec<_>>()
    );
}

/// **⚠ THE SAME DESTRUCTION REACHED FROM ONE COMMAND LINE — END TO END, over the shipped binary.**
///
/// The sibling above plants the colliding name in the store FIRST, so each half's plan-time
/// pre-check can see it. This one plants NOTHING: both names arrive from the same invocation, the
/// store holds neither when the run starts, and `--recorder` supplies `default` from its own
/// `DEFAULT_PROFILE_NAME` with nothing typed twice. Measured at `bab92f0ee`, before the fix:
///
/// ```text
/// PROBE dry:   code=Some(0)   would mirror … run `default` …, recorder `default` … — NOTHING WAS WRITTEN.
/// PROBE apply: code=Some(1)   the settings store already holds a `run` profile called `default` … NOTHING WAS WRITTEN.
/// PROBE store after: [("default", Run)]
/// ```
///
/// Both printed sentences were false: the rehearsal promised a write the store then refused, and
/// the refusal's headline printed over a committed `run` body. The last line is what this test
/// asserts the hardest — **the store must be EMPTY afterwards**, because a check placed anywhere
/// below the plans would still leave that row.
///
/// It drives the SHIPPED binary rather than `execute`, deliberately: the defect is an ORDERING one
/// between halves of one run, and the exit code an operator's script branches on is part of it.
#[test]
fn one_invocation_may_not_name_one_profile_under_two_kinds() {
    let dir = migrated_settings_dir();
    let run_profile = dir.path().join("run-live.toml");
    std::fs::write(
        &run_profile,
        "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 100.0\nmax_total_exposure = 500.0\n",
    )
    .expect("write the run profile");
    let rec = dir.path().join("rec.toml");
    std::fs::write(&rec, RECORDER_PROFILE).expect("write the recorder profile");

    for extra in ["--dry-run", ""] {
        let mut owned: Vec<String> = ["config", "mirror", "--no-settings", "--profile"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        owned.push(run_profile.to_str().expect("utf-8 temp path").to_string());
        owned.push("--profile-name".to_string());
        owned.push("default".to_string());
        owned.push("--recorder".to_string());
        owned.push(rec.to_str().expect("utf-8 temp path").to_string());
        if !extra.is_empty() {
            owned.push(extra.to_string());
        }
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let out = run(dir.path(), &args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "one name under two kinds must FAIL (extra: {extra:?}): {}{}",
            stdout(&out),
            stderr(&out)
        );
        let err = stderr(&out);
        for needle in ["`run` profile", "`recorder` profile", "default", "NOTHING WAS WRITTEN"] {
            assert!(err.contains(needle), "the refusal must name {needle:?}: {err}");
        }
        assert!(
            !stdout(&out).contains("would mirror"),
            "a rehearsal may not promise a write the store refuses: {}",
            stdout(&out)
        );
    }

    // THE ASSERTION THE OLD BEHAVIOUR FAILED: nothing at all is in the profile plane. Before the
    // fix this read `[("default", Run)]` while the command had just said NOTHING WAS WRITTEN.
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("the store must still read");
    assert!(
        profiles.all().is_empty(),
        "no half of a refused run may land: {:?}",
        profiles.all().iter().map(|p| (&p.row.name, p.row.kind)).collect::<Vec<_>>()
    );

    // …and the same two documents under DIFFERENT names are an ordinary success, so the refusal
    // above is about the collision and not about naming two planes in one command.
    let owned: Vec<String> = [
        "config",
        "mirror",
        "--no-settings",
        "--profile",
        run_profile.to_str().expect("utf-8"),
        "--profile-name",
        "run-live",
        "--recorder",
        rec.to_str().expect("utf-8"),
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    let out = run(dir.path(), &args);
    assert!(out.status.success(), "two distinct names must mirror: {}", stderr(&out));
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("read back");
    assert_eq!(profiles.all().len(), 2, "both bodies landed");
}

/// **⚠ THE WRITE PHASE IS NOT ONE TRANSACTION, AND THE REPORT NOW SAYS SO.**
///
/// A `subscription.backfill` word outside the schema's `CHECK` is the one store refusal this verb
/// cannot foresee — `crate::cmd::config_mirror_recorder`'s lowering carries the string through
/// unvalidated and it round-trips, so the recorder half plans clean and the DATABASE is what
/// refuses it. Put a `--profile` ahead of it and the run body has already committed in its own
/// transaction when that happens.
///
/// Before this landing that run printed the store's refusal alone, which is written from inside
/// ONE transaction and is true of it — leaving an operator to read *nothing was written* over a
/// `run` body on disk. Now the run scope is the verb's to state: the sentence is re-scoped to the
/// write it is about, and the report names what landed.
///
/// ⚠ This test deliberately does NOT assert that the `backfill` word is refused at plan time. It
/// is a real gap and it is not this commit's to close — it is also the only deterministic fault
/// injector in the tree for the state this report exists to describe, so closing it silently would
/// take the coverage with it.
#[test]
fn a_fault_after_a_half_has_committed_names_what_landed_and_rescopes_the_claim() {
    let dir = migrated_settings_dir();
    let run_profile = dir.path().join("run-live.toml");
    std::fs::write(&run_profile, "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 100.0\n")
        .expect("write the run profile");
    let rec = dir.path().join("rec.toml");
    std::fs::write(
        &rec,
        "store = \"market_data/hist\"\n\n[[subscribe]]\nvenue = \"binance\"\n\
         symbols = [\"BTCUSDT.P\"]\nbackfill = \"sometimes\"\n",
    )
    .expect("write a recorder profile the SCHEMA refuses");

    let out = run(
        dir.path(),
        &[
            "config",
            "mirror",
            "--no-settings",
            "--profile",
            run_profile.to_str().expect("utf-8"),
            "--recorder",
            rec.to_str().expect("utf-8"),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "the store refuses it: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("PART OF THIS RUN WAS ALREADY WRITTEN"), "{err}");
    assert!(err.contains("run profile `run-live`"), "it names what landed: {err}");
    assert!(err.contains("recorder profile `default`"), "…and what failed: {err}");

    // THE HALF THAT MATTERS: the run body really is on disk, so a report claiming otherwise would
    // be the defect this verb was audited for.
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("the store must still read");
    assert_eq!(
        profiles.all().iter().map(|p| p.row.name.as_str()).collect::<Vec<_>>(),
        vec!["run-live"],
        "the earlier half committed in its own transaction"
    );
}

/// **Ruling 3: `--addr` is ACCEPTED and refused BY NAME on every verb**, so an operator who asks
/// for the remote route is told the route is unbuilt rather than that the flag does not exist.
#[test]
fn record_refuses_the_remote_route_by_name_on_every_verb() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for argv in [
        vec!["data", "realtime", "record", "ls", "--addr", "1.2.3.4:9"],
        vec!["data", "realtime", "record", "add", "binance:BTCUSDT", "--addr", "1.2.3.4:9"],
        vec!["data", "realtime", "record", "rm", "binance:BTCUSDT", "--addr=1.2.3.4:9"],
    ] {
        let out = run(scratch.path(), &argv);
        assert_eq!(out.status.code(), Some(2), "{argv:?} is a usage error: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("designed and not built"), "{argv:?}: {err}");
        assert!(!err.contains("unknown option"), "a real flag is not an unknown one: {err}");
    }
    // ...and `--lane`, the word this group's SIBLING verb owns, is refused with the reserved one.
    let out = run(
        scratch.path(),
        &["data", "realtime", "record", "add", "binance:BTCUSDT", "--lane", "trades"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--stream"), "{}", stderr(&out));
}

/// **THE END-TO-END: `add` → `ls` → `rm`, through the SHIPPED binary and a real SQLite store.**
///
/// What no unit test can see: that the row genuinely lands in `<settings>/db/vike.db`, that
/// `vike-cli config recorder` — the verb an operator greps with — reads back what this verb wrote,
/// and that a `--dry-run` really writes nothing.
#[test]
fn record_add_and_rm_round_trip_through_the_real_store() {
    let dir = recorder_settings_dir("default", true);

    // A DRY RUN first: it must print the plan and change nothing.
    let out =
        run(dir.path(), &["data", "realtime", "record", "add", "binance:ETHUSDT.P", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("NOTHING was written"), "{text}");
    assert!(text.contains("+ ord 2"), "the plan names the row it would write: {text}");
    let after_dry = stdout(&run(dir.path(), &["data", "realtime", "record", "ls"]));
    assert!(!after_dry.contains("ETHUSDT.P"), "a dry run must write nothing: {after_dry}");

    // ...then the real one.
    let out = run(
        dir.path(),
        &[
            "data",
            "realtime",
            "record",
            "add",
            "binance:ETHUSDT.P",
            "--backfill",
            "venue",
            "--note",
            "added by the round-trip case",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("NEXT RESTART"), "ruling 2 rides every write: {text}");
    assert!(text.contains("--record <path>"), "ruling 6 rides it too: {text}");
    // The venue probe could not reach a datahub, so the row was written UNCHECKED and says so.
    assert!(text.contains("UNCHECKED"), "the degrade is DISCLOSED, never silent: {text}");

    // It is in the store, and the verb an operator greps with reads it back.
    let listed = stdout(&run(dir.path(), &["config", "recorder"]));
    assert!(listed.contains("binance symbols [\"ETHUSDT.P\"]"), "{listed}");
    assert!(listed.contains("added by the round-trip case"), "the NOTE survived: {listed}");
    // …and so did the rows the mirror wrote, which is the whole point of the row-based write path.
    assert!(listed.contains("polymarket family btc-updown-5m"), "{listed}");

    // The JSON form carries `symbols` as an ARRAY rather than the stored TOML text.
    let doc: serde_json::Value = serde_json::from_str(&stdout(&run(
        dir.path(),
        &["data", "realtime", "record", "ls", "--json"],
    )))
    .expect("one document");
    assert_eq!(doc["profile"], "default");
    assert_eq!(doc["active"], true);
    let rows = doc["subscriptions"].as_array().expect("rows").clone();
    assert_eq!(rows.len(), 3, "{doc}");
    assert_eq!(rows[2]["symbols"][0], "ETHUSDT.P");
    assert_eq!(rows[2]["backfill"], "venue");
    assert!(rows[0]["symbols"].is_null(), "a family row has no symbols: {doc}");

    // A DUPLICATE is refused by name rather than written twice.
    let out = run(dir.path(), &["data", "realtime", "record", "add", "binance:ETHUSDT.P"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("already a subscription"), "{}", stderr(&out));

    // ...and `rm` takes it out again.
    let out = run(dir.path(), &["data", "realtime", "record", "rm", "binance:ETHUSDT.P"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("- ord 2"), "{}", stdout(&out));
    let after = stdout(&run(dir.path(), &["data", "realtime", "record", "ls"]));
    assert!(!after.contains("ETHUSDT.P"), "{after}");
    assert!(after.contains("family btc-updown-5m"), "the other rows are untouched: {after}");

    // A SECOND `rm` of the same spec is a refusal rather than a silent success — a removal that
    // reported success while removing nothing is the failure ruling 5 exists to prevent.
    let out = run(dir.path(), &["data", "realtime", "record", "rm", "binance:ETHUSDT.P"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("no subscription"), "{}", stderr(&out));
}

/// **`@` IS A FAMILY ON THIS VERB AND AN ORDINARY SYMBOL ON ITS SIBLING**, end to end through the
/// binary — the one character this group reads two ways, which is why the two parsers may not be
/// shared.
#[test]
fn the_at_marker_is_a_family_on_record_and_an_ordinary_symbol_on_watch() {
    let dir = recorder_settings_dir("default", true);
    let out = run(dir.path(), &["data", "realtime", "record", "add", "hyperliquid:@PURR"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = stdout(&run(dir.path(), &["config", "recorder"]));
    assert!(listed.contains("hyperliquid family PURR"), "it is a FAMILY here: {listed}");

    // The sibling verb reads the same token as a SYMBOL and hands it to the wire verbatim, so it
    // gets as far as the DIAL — a connect failure here, never a usage error.
    let out = run(
        dir.path(),
        &["data", "realtime", "watch", "hyperliquid:@PURR", "--lane", "trades", "--events", "1"],
    );
    assert_ne!(out.status.code(), Some(2), "`@` is not a usage error on watch: {}", stderr(&out));
}
