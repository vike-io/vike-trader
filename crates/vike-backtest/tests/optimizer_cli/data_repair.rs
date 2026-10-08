//! `data repair`: the verb that gives the manifest rebuild an operator.

use super::*;

// ---- `data repair`: the verb that gives the manifest rebuild an operator ----------------------

/// ⚠ **A WILDCARD is refused, and the refusal must carry the argument rather than the rule.** `rm`
/// wildcards an omitted dimension; `repair` cannot, because the failure it fixes — a series whose
/// base manifest is gone — is invisible to `DataFusionHist::list_series` (which finds leaves BY that
/// file), because the rebuild holds one series' lock across every part footer it reads, and because
/// its verdict is per-series. "Why can't I repair a whole venue" is the first question an operator
/// asks, so the answer is in the message.
///
/// ⚠ Refused BEFORE the store is opened, which is asserted on the filesystem: `DataFusionHist::open`
/// creates whatever root it is handed, and `optimizer_cli.rs`'s
/// `a_refused_flag_never_opens_the_store` is the rule this extends to the new verb. (`repair` itself
/// uses `open_read_only`, which creates nothing either — this pins that a refusal precedes even
/// that.)
#[test]
fn data_repair_refuses_a_wildcard_before_it_opens_a_store() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("never");
    let never = path.to_str().expect("a UTF-8 scratch path");
    for argv in [
        // No symbol and no group at all.
        vec!["data", "repair", "--kind", "bar", "--venue", "binance", "--store", never, "--json"],
        // ⚠ A bar symbol with NO `--interval` is ALSO a wildcard — over every interval that symbol
        // was recorded at — and only `STORE_KINDS` knows that, which is why this half is refused
        // HERE and not in `vike-cli`.
        vec![
            "data", "repair", "--kind", "bar", "--venue", "binance", "--symbol", "BTCUSDT",
            "--store", never, "--json",
        ],
    ] {
        let out = run(&argv);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{argv:?}: {stderr}");
        assert!(stderr.contains("names more than one series"), "{argv:?}: {stderr}");
        assert!(stderr.contains("per-series"), "the ARGUMENT, not just the rule: {stderr}");
        assert!(
            !Path::new(never).exists(),
            "a wildcard must be refused before a store is opened, but {never:?} was created by \
             {argv:?}"
        );
    }
}

/// A TICK series is fully named by `--symbol` alone — it has no `interval=` segment — so the same
/// refusal must NOT fire there. Without this the wildcard rule would be a rule about flag COUNTS
/// rather than about the store's layout, and it would demand an `--interval` on a kind that has
/// none.
#[test]
fn data_repair_accepts_a_tick_series_named_by_symbol_alone() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    std::fs::create_dir_all(&store).expect("the store root");
    let out = run(&[
        "data",
        "repair",
        "--kind",
        "trade",
        "--venue",
        "binance",
        "--symbol",
        "BTCUSDT",
        "--store",
        store.to_str().expect("a UTF-8 path"),
    ]);
    let stderr = stderr_of(&out);
    assert!(
        !stderr.contains("names more than one series"),
        "a tick series is fully named by --symbol: {stderr}"
    );
    // It gets as far as the LEAF check and refuses there instead, which is the next rung and the
    // proof that the selector itself was accepted.
    assert!(stderr.contains("no series directory at"), "{stderr}");
}

/// ⚠ **A MISTYPED selector must not MINT a series.** `rebuild_manifest` reads an absent directory
/// as an empty series and the series lock would CREATE the leaf, so a rebuild driven straight off a
/// typo would publish an empty manifest at a path nothing ever wrote — and `list_series`, which
/// finds leaves by that very file, would enumerate the phantom forever. Asserted on the filesystem.
#[test]
fn data_repair_refuses_a_series_that_is_not_there_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    std::fs::create_dir_all(&store).expect("the store root");
    let out = run(&[
        "data",
        "repair",
        "--kind",
        "bar",
        "--venue",
        "binanc",
        "--symbol",
        "BTCUSDT",
        "--interval",
        "1m",
        "--yes",
        "--store",
        store.to_str().expect("a UTF-8 path"),
    ]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("no series directory at"), "{stderr}");
    assert!(stderr.contains("missing from `vike-cli data hist ls`"), "the trap is named: {stderr}");
    assert!(
        !store.join("kind=bar").join("venue=binanc").exists(),
        "a refused repair must not create the leaf it was pointed at by mistake"
    );
}

/// ⚠ **THE REHEARSAL DEFAULT, through the shipped binary.** A line with neither `--yes` nor
/// `--dry-run` prints the plan, writes NOTHING, and exits 0 — and says so in as many words, because
/// an exit-0 run that did nothing is the one place this verb could hand somebody a false green.
///
/// The store is seeded by `data seed-demo`, which needs no network and no feature beyond the one
/// this test binary already requires.
#[test]
fn data_repair_rehearses_by_default_and_says_nothing_was_written() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    let store_arg = store.to_str().expect("a UTF-8 path");
    assert!(run(&["data", "seed-demo", "--store", store_arg]).status.success());

    let series =
        store.join("kind=bar").join("venue=demo").join("symbol=BTCUSDT").join("interval=1h");
    assert!(series.is_dir(), "the demo tape writes this series: {}", series.display());
    let manifest = series.join("_manifest.json");
    let before = std::fs::read(&manifest).expect("the seeded manifest");

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
        "--store",
        store_arg,
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "a rehearsal is a SUCCESS: {}", stderr_of(&out));
    assert!(stdout.contains("store: "), "the resolved root LEADS: {stdout}");
    assert!(stdout.contains("rebuild would:"), "{stdout}");
    assert!(stdout.contains("critical section:"), "the lock hold is quantified: {stdout}");
    assert!(stdout.contains("NOTHING WAS WRITTEN"), "an exit-0 no-op must say so: {stdout}");
    assert_eq!(
        std::fs::read(&manifest).expect("the manifest"),
        before,
        "a rehearsal must leave the manifest BYTE-identical"
    );

    // ...and `--yes` performs it, on the same store, reporting the verdict in the past tense.
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
        "--yes",
        "--store",
        store_arg,
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "a LOSSLESS rebuild is a clean exit: {}", stderr_of(&out));
    assert!(stdout.contains("verdict: LOSSLESS"), "{stdout}");
    assert!(stdout.contains("rebuilt "), "{stdout}");
}

/// ⚠ **A LOSSY SUCCESS IS NOT A CLEAN EXIT.** The rebuild worked and the rows read again, so this
/// is not a failure — but exiting 0 over a series that just lost its idempotency log is what sends
/// an operator into a backfill that DUPLICATES rows. The exit code is the only thing a wrapper
/// reads, so it is what has to say so.
///
/// ⚠ **The lossy state staged here is ORPHAN COMMIT KEYS, and it is chosen because it is the loss
/// no `RebuildReport` field can carry.** A rebuild derives keys from part FOOTERS, so a key that no
/// part carries — `Manifest::orphan_commits`, the v2 migration's residue, of which the live box's
/// largest series had 13 — simply disappears, counted by none of the five. If the verdict were
/// computed from the report alone this rebuild would read CLEAN, which is precisely the failure
/// this test exists to prevent. It is also the only lossy shape a test can stage from OUTSIDE the
/// store: the base is plain JSON and the field is `#[serde(default)]`, while a keyless PART needs
/// an append this binary has no verb for.
#[test]
fn a_lossy_data_repair_exits_non_zero_and_names_the_consequence() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    let store_arg = store.to_str().expect("a UTF-8 path");
    assert!(run(&["data", "seed-demo", "--store", store_arg]).status.success());
    let series =
        store.join("kind=bar").join("venue=demo").join("symbol=BTCUSDT").join("interval=1h");

    // The v2 migration's own output shape, written by hand: keys the base records and no part
    // carries. See this test's doc for why THIS lossy shape and not another.
    let manifest = series.join("_manifest.json");
    let text = std::fs::read_to_string(&manifest).expect("the seeded manifest");
    let mut doc: serde_json::Value = serde_json::from_str(&text).expect("valid manifest JSON");
    doc["orphan_commits"] = serde_json::json!(["live-2026-08-02:1", "live-2026-08-02:2"]);
    std::fs::write(&manifest, serde_json::to_vec_pretty(&doc).expect("re-serialize"))
        .expect("write the migrated-shaped manifest");

    // The REHEARSAL already calls it lossy, which is the point of rehearsing.
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
        "--store",
        store_arg,
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "a rehearsal is a success whatever its verdict");
    assert!(stdout.contains("verdict: LOSSY"), "{stdout}");
    assert!(stdout.contains("2 commit key(s)"), "{stdout}");

    // ...and performing it exits NON-ZERO, with the losses named.
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
        "--yes",
        "--store",
        store_arg,
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = stderr_of(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a LOSSY rebuild must NOT read as a clean exit; stdout: {stdout}"
    );
    assert!(stdout.contains("verdict: LOSSY"), "{stdout}");
    assert!(stdout.contains("orphan key(s) dropped"), "{stdout}");
    assert!(stderr.contains("SUCCEEDED and was LOSSY"), "the exit is EXPLAINED: {stderr}");
}

/// The `--json` document carries every `RebuildReport` count, the verdict a wrapper branches on,
/// and whether anything was written — so a caller never has to read the prose to learn either.
#[test]
fn the_data_repair_json_document_carries_the_counts_and_the_verdict() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    let store_arg = store.to_str().expect("a UTF-8 path");
    assert!(run(&["data", "seed-demo", "--store", store_arg]).status.success());

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
        "--store",
        store_arg,
        "--json",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout must be the document alone: {e}\n{:?}", out.stdout));
    assert!(doc["store_root"].is_string(), "{doc}");
    assert!(doc["store_rung"].is_string(), "{doc}");
    // Every one of the five, by name — the counts an operator MUST see.
    for field in [
        "parts_recovered",
        "parts_without_keys",
        "parts_unreadable",
        "parts_superseded",
        "parts_unpublished_merge",
    ] {
        assert!(doc["plan"]["report"][field].is_u64(), "the document must carry {field}: {doc}");
    }
    assert_eq!(doc["lossless"], serde_json::json!(true), "{doc}");
    assert!(doc["losses"].as_array().expect("an array").is_empty(), "{doc}");
    assert_eq!(doc["written"], serde_json::Value::Null, "a rehearsal wrote nothing: {doc}");
    // ⚠ The plan goes to STDERR under --json rather than nowhere: stdout is the document and
    // nothing else, but a run must still have SHOWN an operator what it would cost.
    assert!(stderr_of(&out).contains("rebuild would:"), "{}", stderr_of(&out));
}
