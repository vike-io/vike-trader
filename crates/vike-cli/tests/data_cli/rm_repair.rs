//! `rm` and `repair`: the destructive verb's refusals, and both reaching the engine as flags.

use super::support::spawn_seeded_datahub;
use super::*;

// ─── `rm`: the destructive verb ─────────────────────────────────────────────────────────────────

/// `data hist rm --kind bar --venue b`, then `rest`: the command line most refusal rows below
/// start from once they are past the two missing-selector rows.
fn rm_bar_b(rest: &[&'static str]) -> Vec<&'static str> {
    let mut args = vec!["data", "hist", "rm", "--kind", "bar", "--venue", "b"];
    args.extend_from_slice(rest);
    args
}

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
        (rm_bar_b(&["--symbol", "S", "--group", "G"]), "ALTERNATIVES"),
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
        (rm_bar_b(&["--symbol", "BTC*"]), "glob character"),
        (rm_bar_b(&["--symbol", " "]), "EMPTY value"),
        // ⚠ **The blank PREFIX, in both spellings, on both routes.** There was no row for it —
        // the loop above happened to catch it, with a sentence about the store's grouped-series
        // `symbol=` sentinel that is false of a provenance assertion and names the opposite
        // remedy. The needle is the CONSEQUENCE, so the wrong refusal cannot satisfy it.
        (rm_bar_b(&["--produced-by", ""]), "matches every key"),
        (rm_bar_b(&["--produced-by="]), "matches every key"),
        (rm_bar_b(&["--produced-by", "", "--addr", "127.0.0.1:1"]), "matches every key"),
        // ⚠ A PRODUCER PATH under `--addr`. The SERVER half landed on 2026-09-11, so this is a
        // COMPATIBILITY guard now rather than a stand-in: this protocol carries no capability
        // string for "this server resolves producer paths", so a datahub that has not been
        // redeployed still asserts one as a literal prefix, matches no key, and reports the store
        // as foreign.
        (
            rm_bar_b(&["--produced-by", "crates/vike-data/src/demo.rs", "--addr", "127.0.0.1:1"]),
            "PRODUCER PATH",
        ),
        (rm_bar_b(&["--name", "x"]), "LISTING"),
        (rm_bar_b(&["--days", "3"]), "FETCH window"),
        (rm_bar_b(&["b:S:1h"]), "takes no"),
        (rm_bar_b(&["--addr", "h:1", "--store", "/s"]), "two DIFFERENT stores"),
        // ⚠ THE ONE. Fully named, so no `--produced-by` is needed, and the command line is
        // otherwise perfect — it is refused because nobody can confirm it.
        (rm_bar_b(&["--symbol", "S", "--interval", "1h"]), "not a terminal"),
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
