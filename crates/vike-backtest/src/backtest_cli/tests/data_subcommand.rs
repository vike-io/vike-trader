use super::*;

fn argv(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_string()).collect()
}

/// Every retired flag names a live replacement, in both written spellings, and refuses. The
/// TABLE is what is iterated, so a sixth retirement joins by adding a row.
#[test]
fn every_retired_data_flag_names_its_replacement() {
    for (flag, _, replacement, _) in RETIRED_DATA_FLAGS {
        assert!(
            replacement.starts_with("vike-cli data hist "),
            "{flag} must name the verb the ruling moved it to, in the GROUPED spelling \
                 `vike-cli` accepts (the flat `vike-cli data <verb>` is refused since the group \
                 split), got {replacement:?}"
        );
        for written in [(*flag).to_string(), format!("{flag}=x")] {
            assert!(
                refuse_a_retired_data_flag(std::slice::from_ref(&written)).is_some(),
                "{written} must be refused, not read as absent"
            );
        }
    }
    assert!(
        refuse_a_retired_data_flag(&argv(&["--json"])).is_none(),
        "an unrelated flag falls through"
    );
}

/// ⚠ The adjacent-prefix pair, pinned: `--fetch-starter` must refuse under its OWN row, naming
/// its own replacement, rather than under `--fetch`'s. `has_flag` is exact-token and `arg` is
/// anchored on `--fetch=`, so this holds — and it is the property that would break first if
/// either helper were loosened.
#[test]
fn fetch_starter_is_not_swallowed_by_the_fetch_row() {
    let args = argv(&["--fetch-starter"]);
    let hit = RETIRED_DATA_FLAGS.iter().find(|(f, _, _, _)| flag_given(&args, f));
    assert_eq!(hit.map(|(f, _, _, _)| *f), Some("--fetch-starter"));
}

/// ⚠ **The engine subcommand a retirement names must be one [`DATA_SUBS`] actually serves.**
///
/// It was DERIVED (`flag.trim_start_matches("--")`), which is right for four rows and wrong for
/// the fifth: `--rm-series` yielded `rm-series`, so the refusal for the one retired flag that
/// DELETES told an engine-only operator to run `backtest data rm-series`, which
/// [`triage_data_argv`] answers with "unknown `data` subcommand". The end-to-end test could not
/// catch it — it asserts the message CONTAINS the sub, and `"rm-series"` contains `"rm"` — so
/// the check has to be against the table that decides rather than against the message.
#[test]
fn the_retired_sub_is_a_real_data_subcommand() {
    for (flag, sub, _, _) in RETIRED_DATA_FLAGS {
        assert!(
            DATA_SUBS.iter().any(|(name, _)| name == sub),
            "{flag} names `backtest data {sub}`, which is not a subcommand this binary serves \
                 ({:?})",
            DATA_SUBS.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        );
    }
}

/// The triage refuses SHAPE and nothing else: an unknown option, a boolean given a value, a
/// valued flag given none, and the positional's presence-or-absence per subcommand.
#[test]
fn the_triage_refuses_shape_and_defers_every_value() {
    assert_eq!(
        triage_data_argv("fetch", &argv(&["binance:BTCUSDT:1h", "--days", "180"])).unwrap(),
        Some("binance:BTCUSDT:1h".to_string())
    );
    assert_eq!(triage_data_argv("seed-demo", &argv(&[])).unwrap(), None);
    // The value is not judged here — a nonsense venue and a nonsense window both pass, and the
    // arm's own error names what it could not do.
    assert_eq!(
        triage_data_argv("fetch", &argv(&["nope:NOPE:99z", "--days", "-4"])).unwrap(),
        Some("nope:NOPE:99z".to_string())
    );

    assert!(
        triage_data_argv("nope", &argv(&[])).unwrap_err().contains("unknown `data` subcommand")
    );
    assert!(triage_data_argv("rm", &argv(&["--bogus"])).unwrap_err().contains("unknown option"));
    assert!(triage_data_argv("rm", &argv(&["--kind"])).unwrap_err().contains("requires a value"));
    assert!(
        triage_data_argv("rm", &argv(&["--kind", "--venue"])).unwrap_err().contains("another flag")
    );
    assert!(triage_data_argv("export", &argv(&["--out", "x"])).unwrap_err().contains("needs a"));
    assert!(triage_data_argv("seed-demo", &argv(&["x:y:z"])).unwrap_err().contains("takes no"));
}

/// ⚠ **`--store` on the READ verb is refused BY NAME, in both spellings and with no value at
/// all, and on a WRITER it is not.** Decision 0084's 2026-09-25 amendment closed the local READ
/// door; `data export` was the one reader it left open, and this is the door closing. The
/// sentence is `store_flag_removed`'s, so the replacement is spelled here exactly as every
/// other reader spells it — and it names the verb as the operator typed it.
#[test]
fn the_store_flag_on_a_data_read_is_refused_by_name_and_a_writer_keeps_it() {
    for written in [
        vec!["demo:DEMOUSDT:1h", "--out", "x.parquet", "--store", "/srv/hist"],
        vec!["demo:DEMOUSDT:1h", "--out", "x.parquet", "--store=/srv/hist"],
        // A TRAILING value-less `--store`: the triage would answer "requires a value", which
        // asks for a directory that would then be refused. The refusal comes first.
        vec!["demo:DEMOUSDT:1h", "--out", "x.parquet", "--store"],
    ] {
        let why = refuse_a_store_on_a_data_read("export", &argv(&written))
            .unwrap_or_else(|| panic!("{written:?} must be refused"));
        assert!(why.starts_with("backtest data export:"), "names the verb as typed: {why}");
        assert!(why.contains("vike-backend datahub --store DIR"), "the replacement: {why}");
        assert!(why.contains("decision 0084"), "the record, by number: {why}");
    }
    // The same verb WITHOUT the flag passes through — the refusal is about the flag.
    assert!(refuse_a_store_on_a_data_read("export", &argv(&["demo:D:1h", "--out", "x"])).is_none());
    // …and every WRITER keeps its `--store`: the ruling was about readers.
    for writer in DATA_SUBS.iter().map(|(n, _)| *n).filter(|n| !DATA_READS.contains(n)) {
        assert!(
            refuse_a_store_on_a_data_read(writer, &argv(&["--store", "/srv/hist"])).is_none(),
            "`data {writer}` WRITES and keeps its --store"
        );
    }
}

/// Every reader row names a real subcommand, so the refusal cannot guard a verb that does not
/// exist while the real one reads a directory.
#[test]
fn every_data_read_is_a_real_data_subcommand() {
    for read in DATA_READS {
        assert!(
            DATA_SUBS.iter().any(|(name, _)| name == read),
            "DATA_READS names `{read}`, which DATA_SUBS does not serve"
        );
    }
}

/// ⚠ **The `--dry-run=1` hole, and it is the reason this triage exists at all.** `has_flag` is
/// exact-token, so `--dry-run=1` was silently ignored while `--yes` beside it was honoured —
/// a command line written as a rehearsal performed the deletion. It is a REFUSAL now, and the
/// message says what it used to do.
#[test]
fn a_boolean_given_a_value_is_refused_rather_than_ignored() {
    for spelling in ["--dry-run=1", "--yes=true", "--json=1"] {
        let err = triage_data_argv("rm", &argv(&["--kind", "bar", "--venue", "x", spelling]))
            .unwrap_err();
        assert!(err.contains("takes no value"), "{spelling}: {err}");
    }
    let err =
        triage_data_argv("rm", &argv(&["--kind", "bar", "--venue", "x", "--dry-run=1", "--yes"]))
            .unwrap_err();
    assert!(err.contains("rehearsal"), "the message says what it used to do: {err}");
}
