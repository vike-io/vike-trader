//! The spellings the `store` verbs accept and the ones they refuse before a store opens, over the
//! shipped binary.

use super::*;

/// The verb group answers help on stdout with exit 0, in every spelling, and a bare one too.
#[test]
fn the_store_verb_answers_help_on_stdout_with_exit_zero() {
    for args in [vec!["store"], vec!["store", "--help"], vec!["store", "-h"]] {
        let out = run(&args);
        assert!(out.status.success(), "{args:?}: {:?}", stderr_of(&out));
        let stdout = stdout_of(&out).to_lowercase();
        assert!(stdout.contains("usage"), "{args:?}: {stdout:?}");
        assert!(stderr_of(&out).trim().is_empty(), "{args:?}: {:?}", stderr_of(&out));
    }
}

/// The offline writers, in the spellings `vike-cli data history` spawns (`--store` last, the selector
/// flags spaced) and the inline ones an operator may type.
#[test]
fn the_offline_data_verbs_accept_every_spelling_they_accepted() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    let store = store.to_str().expect("a UTF-8 path");

    seed_test_tape(Path::new(store));
    // A negative number is a value (`--from -4`), where a flag may not be one: it parses, and fails
    // only at the datahub read.
    let out = run(&["store", "export", "demo:BTCUSDT:1h", "--out", "x.parquet", "--from", "-4"]);
    assert!(stderr_of(&out).contains("reading demo:BTCUSDT:1h from"), "{}", stderr_of(&out));

    // The inline `--store=` spelling, on a writer.
    let inline_flag = format!("--store={store}");
    let out = run(&[
        "store",
        "repair",
        "--kind=bar",
        "--venue=demo",
        "--symbol=BTCUSDT",
        "--interval=1h",
        "--dry-run",
        inline_flag.as_str(),
    ]);
    assert!(out.status.success(), "repair --store=: {:?}", stderr_of(&out));

    // `rm --dry-run` over what was seeded: the plan prints and nothing is deleted.
    for args in [
        vec![
            "store",
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
            "store",
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

    // `repair` rehearses by default (`vike-cli data history repair` forwards `--dry-run` or `--yes`).
    let out = run(&[
        "store",
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
        "store",
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

/// A value-less `--store` on a writer refuses naming the verb, and opens nothing.
#[test]
fn a_value_less_store_on_a_writer_is_refused_by_name() {
    let scratch = Scratch::project();
    let out = scratch.tool().args(["store", "repair", "--store"]).output().expect("the tool");
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("store repair: --store requires a value"), "{stderr}");
}

/// **The widening's blank-value audit, at the one flag that guards an IRREVERSIBLE delete.**
///
/// For a flag whose ABSENCE carries meaning, a `--flag=` token answers `Some("")` and changes the
/// answer — and `--produced-by` is the worst case in the tree, because a blank prefix does not match
/// nothing, it matches EVERYTHING. `vike_data::store::store_kind::key_matches_prefix` is `starts_with`,
/// so `vike_data::store::removal::RemovalPlan::verdict` finds no foreign key in any series and the
/// provenance assertion passes VACUOUSLY; and because the value is `Some` rather than `None`,
/// `crates/vike-datahub/src/datahub_cli/store_cmd.rs`'s `run_rm_series` sweep gate — the rule that
/// REQUIRES provenance before a wildcard delete — stands down too. `store rm --kind bar --venue binance
/// --produced-by=$PREFIX --yes` with `$PREFIX` unset used to refuse; it would have deleted every
/// matched series instead.
///
/// The refusal lives in `vike_data::store::store_kind::resolve_produced_by`, one site below every caller,
/// which is why the SPACED form is closed by the same edit.
///
/// `--json` because the `rm` arm leads with its resolved store root on STDOUT for a human and on STDERR
/// under `--json`, and [`refuses`] requires an empty stdout. The store path must still not exist
/// afterwards: this refusal is resolved BEFORE `HistoryStoreDataFusion::open`, which `create_dir_all`s whatever
/// it is handed.
#[test]
fn a_blank_produced_by_is_refused_rather_than_asserting_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("never");
    let never = path.to_str().expect("a UTF-8 scratch path");
    // A WILDCARD selector — no symbol, no group, no interval — which is the shape `--produced-by`
    // is REQUIRED for and therefore the shape a vacuous assertion sets loose. `--json` puts the
    // arm's leading `store:` line on stderr, which is what [`refuses`] needs to see an empty stdout.
    let base = ["store", "rm", "--kind", "bar", "--venue", "binance", "--store", never, "--json"];
    for blank in [vec!["--produced-by="], vec!["--produced-by", ""]] {
        let argv: Vec<&str> = base.iter().chain(blank.iter()).copied().collect();
        // The blank is refused by `Program` for every argument at the parse (decision 0155), one
        // step ABOVE `resolve_produced_by`'s "matches every key" sentence, which still guards a
        // caller that does not go through `Program`.
        refuses(&argv, &["--produced-by", "non-empty"]);
        assert!(
            !Path::new(never).exists(),
            "a blank --produced-by must be refused before the store is opened, but {never:?} was \
             created by {argv:?}"
        );
    }

    // …and the flag's ABSENCE on the same wildcard selector is still the OTHER refusal, by name —
    // so the blank check did not merely relabel a rule that was already firing.
    refuses(&base, &["--produced-by is REQUIRED"]);
}

/// **`store rm --source` takes the lane and leaves the archive beside it**, with its dry run first;
/// without the flag the same line takes the archive and leaves the lane. Only `rm` takes it: on `repair` it would
/// silently address the archive's series.
#[test]
fn rm_names_a_lane_to_reach_it_and_only_rm_takes_the_flag() {
    use vike_data::{HistoryStore, SeriesId};
    use vike_model::StoreKindId;

    let dir = tempfile::tempdir().expect("a scratch directory");
    let root = dir.path().join("hist");
    let store_arg = root.to_str().expect("a UTF-8 scratch path");
    let archive = SeriesId::per_symbol(StoreKindId::Quote, "dukascopy", "EURUSD", None);
    let lane = archive.clone().with_source("recorder");
    let open = || HistoryStoreDataFusion::open(&root).expect("open");
    let tick = vike_marketdata::TickQuote {
        timestamp: 1_000,
        local_timestamp: 0,
        bid: 1.1,
        ask: 1.2,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: Default::default(),
    };
    let record = || {
        let row = std::slice::from_ref(&tick);
        open()
            .append_quotes_sourced("dukascopy", "recorder", "EURUSD", row, Some("live-1"))
            .unwrap();
    };
    let row = std::slice::from_ref(&tick);
    open().append_quotes("dukascopy", "EURUSD", row, Some("dukascopy:EURUSD")).unwrap();
    record();

    let select = [
        "store",
        "rm",
        "--kind",
        "quote",
        "--venue",
        "dukascopy",
        "--symbol",
        "EURUSD",
        "--store",
        store_arg,
    ];
    let rm = |extra: &[&'static str]| {
        let mut argv = select.to_vec();
        argv.extend_from_slice(extra);
        run(&argv)
    };

    let dry = rm(&["--source", "recorder", "--dry-run"]);
    assert!(dry.status.success(), "{:?}", stderr_of(&dry));
    let plan = stdout_of(&dry);
    assert!(plan.contains("quote/dukascopy/source=recorder/symbol=EURUSD"), "{plan}");
    assert_eq!(open().list_series().unwrap().len(), 2, "a rehearsal removed nothing");

    let took = rm(&["--source", "recorder", "--yes"]);
    assert!(took.status.success(), "{:?}", stderr_of(&took));
    assert_eq!(open().list_series().unwrap(), [archive], "the lane alone went");

    record();
    let took = rm(&["--yes"]);
    assert!(took.status.success(), "{:?}", stderr_of(&took));
    assert_eq!(open().list_series().unwrap(), [lane], "no flag: the sourceless series alone went");

    let repair = run(&[
        "store",
        "repair",
        "--kind",
        "quote",
        "--venue",
        "dukascopy",
        "--symbol",
        "EURUSD",
        "--source",
        "recorder",
        "--store",
        store_arg,
    ]);
    assert_eq!(repair.status.code(), Some(2), "{:?}", stderr_of(&repair));
    assert!(stderr_of(&repair).contains("--source"), "{:?}", stderr_of(&repair));
}

/// ⚠ **The `--dry-run=1` hole, end to end on the shipped binary.**
///
/// `vike_utility::argv::has_flag` is exact-token, so `--dry-run=1` was SILENTLY IGNORED while the `--yes`
/// beside it was honoured: a command line written as a rehearsal performed the deletion. `clap` refuses
/// it now, before the store is opened — which is why this asserts the store path was never created as
/// well as the exit code.
#[test]
fn a_boolean_written_with_a_value_is_refused_before_the_store_opens() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");
    let out = run(&[
        "store",
        "rm",
        "--kind",
        "bar",
        "--venue",
        "binance",
        "--produced-by",
        "panel_bars:",
        "--store",
        never_str,
        "--dry-run=1",
        "--yes",
    ]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr:?}");
    // `clap` refuses a value on a switch and names the flag.
    assert!(stderr.contains("--dry-run"), "names the flag: {stderr:?}");
    assert!(
        !Path::new(never_str).exists(),
        "argv triage must precede the store open, but {never_str:?} was created"
    );
}

/// ⚠ **SKEW, the new backend under an old `vike-cli`.** An old client spawns `data <verb>` here; this
/// program has no such word, so it refuses at the parse, exit 2, and opens nothing. The reverse half
/// is `crates/vike-backtesthub/tests/engine_cli/optimizer_cli/flags.rs`'s
/// `the_retired_data_verbs_are_refused_and_open_nothing`.
#[test]
fn an_old_clients_data_verb_is_refused_and_opens_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");
    for args in [
        vec!["data", "seed-demo", "--store", never_str],
        vec!["data", "fetch-starter", "--store", never_str],
        vec!["data", "rm", "--kind", "bar", "--venue", "demo", "--yes", "--store", never_str],
        vec!["data", "repair", "--kind", "bar", "--venue", "demo", "--yes", "--store", never_str],
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr_of(&out));
        assert!(!never.exists(), "{args:?} created the store it was refused for");
    }
}

/// A build without the feature describes `fetch-starter` and refuses it naming the feature; one with
/// it never reaches the network on a line that cannot parse. Asserted on the shipped configuration by
/// this one argv: an unknown flag is refused before any download.
#[test]
fn a_bad_fetch_starter_line_is_refused_before_any_download() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");
    let out = run(&["store", "fetch-starter", "--store", never_str, "--bogus"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("--bogus"), "{}", stderr_of(&out));
    assert!(!never.exists(), "a refused line opened a store");
}
