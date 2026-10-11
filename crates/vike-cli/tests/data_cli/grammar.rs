//! The verb's grammar and its WRITE half: help, usage refusals, and the spawned engine's argv.

use std::path::Path;

use super::*;

/// The verb's help is the only place its subcommands are named — it takes no default action — so a
/// help that did not list every one would leave "get some market data" (or "see what data is
/// there") undiscoverable.
///
/// ⚠ **This roster is HAND-WRITTEN and the module-side one is DERIVED, so only this copy can go
/// short.** The unit test beside `crates/vike-cli/src/cmd/data/hist.rs`'s `SUBCOMMANDS` builds its
/// expectation from that const and therefore cannot go short; an integration test
/// cannot see a private const at all. The failure that shape produces is SUBTRACT-ONLY and
/// silent — a sub-verb missing here simply is not checked, and the file stays green — which is the
/// class `crates/vike-ops/tests/hygiene/path_key_gate.rs` is named for. So: **adding a `data` sub-verb
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
        "fetch", "running", "cancel", "import", "export", "get", "ls", "gaps", "coverage",
        "health", "universe", "gate", "rm", "repair",
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
        // `import`'s own, each caught before a socket is opened: both positionals, the wire's own
        // dataset and bar rules, a day that is not one, and `--verify` without a dry run.
        (vec!["data", "hist", "import", "dukascopy-bi5"], "import needs a DATASET"),
        (vec!["data", "hist", "import", "dukascopy-bi5", "eurusd"], "import dataset"),
        (vec!["data", "hist", "import", "dukascopy-bi5", "EURUSD", "--bars", "7m"], "divide"),
        (
            vec!["data", "hist", "import", "dukascopy-bi5", "EURUSD", "--to", "2024-02-30"],
            "calendar",
        ),
        (vec!["data", "hist", "import", "dukascopy-bi5", "EURUSD", "--verify"], "--dry-run"),
        (vec!["data", "hist", "import", "dukascopy-bi5", "EURUSD", "--days", "7"], "--from/--to"),
        (vec!["data", "hist", "fetch", "b:S:1h", "--days", "1", "--bars", "1m"], "import"),
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
/// different store, and an operator who just ran `data hist rm --store /srv/hist` has every reason
/// to expect `data hist ls --store /srv/hist` to work. So it is refused, on the rung that promises
/// re-running unchanged cannot succeed, with the flag that DOES reach a remote store in the
/// message.
///
/// ⚠ Since 2026-09-26 the refusal LEADS with the one sentence every history reader prints
/// (`vike_datahub_client::flag_vocab::store_flag_removed`) — the same one `data hist export --store`
/// prints — rather than a read-verb sentence of its own that spelled the key-less datahub a second
/// time. Asserted through the BINARY, so the sentence an operator reads is the one checked.
#[test]
fn a_store_flag_on_a_read_verb_is_refused_and_points_at_the_datahub() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for sub in ["ls", "coverage"] {
        let out = run(scratch.path(), &["data", "hist", sub, "--store", "/srv/hist"]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{sub}: {err}");
        assert!(err.contains("--store"), "{sub} must name the flag: {err}");
        assert!(err.contains("--addr"), "{sub} must name the flag that does reach one: {err}");
        let shared =
            vike_datahub_client::flag_vocab::store_flag_removed(&format!("data hist {sub}"));
        assert!(err.contains(&shared), "{sub} must print the shared sentence: {err}");
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

    // ⚠ No `--store` on the export any more: it is refused on that verb since 2026-09-26 (decision
    // 0084's amendment) — the engine reads its bars through a datahub — and
    // `export_refuses_the_store_flag_by_name_before_spawning_anything` below is where that is proved.
    let out = run(
        scratch.path(),
        &["data", "hist", "export", "binance:BTCUSDT:1h", "--out", "s.parquet", "--engine", engine],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: data export binance:BTCUSDT:1h --out s.parquet",
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

/// **`--store` on `data hist export` is REFUSED BY NAME and nothing is spawned** — decision 0084's
/// 2026-09-25 amendment closed the local READ door, and `export` was the one reader it had left
/// open. The engine it spawns reads its bars through a datahub now, so the directory the flag
/// named is served by a key-less one started on it, and the refusal says exactly that.
///
/// ⚠ The engine named is a path that DOES NOT EXIST, and that is the discriminator: had the verb
/// reached the spawn it would exit on the connect rung (`3`, "missing engine") rather than the
/// usage one, so exit 2 WITH the ruling's sentence proves the refusal came first. Not `cfg(unix)`:
/// nothing here is planted.
#[test]
fn export_refuses_the_store_flag_by_name_before_spawning_anything() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let absent = scratch.path().join("no-such-engine");
    let absent = absent.to_str().expect("utf-8 temp path");
    let file = scratch.path().join("s.parquet");
    let file = file.to_str().expect("utf-8 temp path");
    for store in [&["--store", "/srv/hist"][..], &["--store=/srv/hist"][..]] {
        let argv = [
            &["data", "hist", "export", "binance:BTCUSDT:1h", "--out", file][..],
            store,
            &["--engine", absent][..],
        ]
        .concat();
        let out = run(scratch.path(), &argv);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{argv:?} is a usage refusal, not a spawn: {err}");
        // The WHOLE sentence, not a fragment of it: the usage page printed after a refusal names
        // `vike-backend datahub --store DIR` too, so a needle that short would pass on
        // any usage error at all.
        let sentence = vike_datahub_client::flag_vocab::store_flag_removed("data hist export");
        assert!(err.contains(&sentence), "the ONE sentence every reader prints: {err}");
        assert!(!Path::new(file).exists(), "and nothing was written");
    }
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
    // NULL: an export names no store since `--store` left that verb (decision 0084's amendment).
    assert!(doc["store"].is_null(), "{doc}");
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
        serde_json::json!(["data", "export", "binance:BTCUSDT:1h", "--out", "s.parquet"]),
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
