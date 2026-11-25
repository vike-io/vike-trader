//! `import`'s grammar: a FORMAT and a DATASET, inclusive-day bounds, and its flag refusals.

use super::*;
use crate::refusals::RetiredDataSpelling;

// ── `import`: a vendor archive, read on the datahub's own box ─────────────────────────────────

/// Midnight UTC of `y-m-d`, in epoch-ms.
fn utc_midnight(y: i64, m: u32, d: u32) -> i64 {
    vike_utility::calendar::days_from_civil(y, m, d) * vike_model::MS_PER_DAY
}

fn import_of(args: &[&str]) -> ImportArgs {
    let mut argv = vec!["history", "import"];
    argv.extend_from_slice(args);
    parse_of(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}")).import.expect("an ImportArgs")
}

/// **The import door's grammar**: a FORMAT and a DATASET, the window defaulting to the whole
/// dataset and the bars to `1m` — and nothing of it left in the fields the other verbs read.
#[test]
fn import_takes_a_format_and_a_dataset_and_defaults_the_rest() {
    let a = parse_of(&["history", "import", "dukascopy-bi5", "EURUSD"]).unwrap();
    assert_eq!(a.sub, Sub::Import);
    assert_eq!(a.addr, vike_config::DEFAULT_DATAHUB_ADDRESS, "it dials the datahub default");
    assert_eq!(a.spec, None, "the FORMAT is moved into ImportArgs, never left as a spec");
    assert_eq!(a.window, None);
    assert!(a.rm.is_none() && a.store.is_none() && a.engine.is_none() && !a.json);
    assert_eq!(
        a.import.expect("an ImportArgs"),
        ImportArgs {
            format: "dukascopy-bi5".to_string(),
            dataset: "EURUSD".to_string(),
            from_day: None,
            to_day: None,
            bars: vec!["1m".to_string()],
            dry_run: false,
            verify: false,
            yes: false,
            replace_holed: false,
            target: None,
            file: None,
            fetch: None,
        }
    );
    let a = parse_of(&[
        "history",
        "import",
        "dukascopy-bi5",
        "EURUSD",
        "--bars",
        "1m,1h",
        "--dry-run",
        "--verify",
        "--yes",
        "--json",
        "--addr",
        "127.0.0.1:9",
    ])
    .unwrap();
    assert!(a.json && a.address_given && a.addr == "127.0.0.1:9");
    let import_arguments = a.import.expect("an ImportArgs");
    assert_eq!(import_arguments.bars, vec!["1m".to_string(), "1h".to_string()]);
    assert!(import_arguments.dry_run && import_arguments.verify && import_arguments.yes);
    assert!(import_of(&["dukascopy-bi5", "EURUSD", "--bars", "none"]).bars.is_empty());
}

/// **`--from` and `--to` are INCLUSIVE DAYS** — `--from D --to D` is the one day D, each bound the
/// midnight of the day it names, where a fetch's `--to` is an instant — and the request carries
/// exactly those two midnights. Either bound stands alone.
#[test]
fn an_imports_bounds_are_inclusive_days_and_reach_the_request_unchanged() {
    let d = utc_midnight(2024, 1, 15);
    let one = import_of(&["dukascopy-bi5", "EURUSD", "--from", "2024-01-15", "--to", "2024-01-15"]);
    assert_eq!((one.from_day, one.to_day), (Some(d), Some(d)), "one day, whole");
    let req = import_request(&one, one.from_day, one.to_day, import::Mode::Import);
    assert_eq!((req.from_day, req.to_day), (Some(d), Some(d)));
    vike_datahub_client::archive::validate_import_spec(&req)
        .expect("a one-day import is a request the wire accepts");
    let from_only = import_of(&["dukascopy-bi5", "EURUSD", "--from", "2024-01-15"]);
    assert_eq!((from_only.from_day, from_only.to_day), (Some(d), None));
    let to_only = import_of(&["dukascopy-bi5", "EURUSD", "--to", &d.to_string()]);
    assert_eq!((to_only.from_day, to_only.to_day), (None, Some(d)), "an epoch-ms midnight");

    for (args, needle) in [
        (&["--from", "2024-01-16", "--to", "2024-01-15"][..], "AFTER --to"),
        (&["--to", "2024-02-30"][..], "not a calendar date"),
        (&["--from", "1705276800001"][..], "not the START of a UTC day"),
        (&["--to", "2024-01-15T00"][..], "YYYY-MM-DD"),
    ] {
        let mut argv = vec!["history", "import", "dukascopy-bi5", "EURUSD"];
        argv.extend_from_slice(args);
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(needle), "{args:?} must say {needle:?}: {e}");
    }
}

/// Only Dukascopy's lane derives bars unasked; a format this side has no row for asks for none, and a
/// named interval reaches the datahub as asked.
#[test]
fn only_dukascopy_derives_bars_unasked() {
    assert_eq!(import_of(&["dukascopy-bi5", "EURUSD"]).bars, vec!["1m".to_string()]);
    assert!(import_of(&["binance-aggtrades", "SOLUSDT"]).bars.is_empty());
    assert!(import_of(&["binance-aggtrades", "SOLUSDT", "--bars", "none"]).bars.is_empty());
    let named = import_of(&["binance-aggtrades", "SOLUSDT", "--bars", "1m"]);
    assert_eq!(named.bars, vec!["1m".to_string()]);
}

/// `--replace-holed` reaches every request of the run, a plan's included (so a dry run lists the days),
/// is left off a request that did not ask, and belongs to `import` alone.
#[test]
fn replace_holed_reaches_every_request_and_belongs_to_import_alone() {
    use import::Mode;
    let asked = import_of(&["binance-klines", "BTCUSDT.P", "--bars", "1m", "--replace-holed"]);
    assert!(asked.replace_holed);
    for mode in [Mode::Plan, Mode::Verify, Mode::Import] {
        assert!(import_request(&asked, None, None, mode).replace_holed, "{mode:?}");
    }
    let plain = import_of(&["binance-klines", "BTCUSDT.P", "--bars", "1m"]);
    assert!(!import_request(&plain, None, None, Mode::Import).replace_holed);
    let e =
        parse_of(&["history", "fetch", "binance:BTCUSDT.P:1m", "--days", "1", "--replace-holed"])
            .unwrap_err();
    assert!(e.contains("--replace-holed") && e.contains("data history import"), "{e}");
}

/// `csv-bars` alone is told its series, by all four flags, held to the wire's rules before a socket
/// opens; the lane it writes, named whole by `--lane user-<label>`, is read back by `get --lane` and
/// `export --lane`; and among the import formats `--lane` belongs to it alone.
#[test]
fn csv_bars_takes_its_series_from_four_flags_and_get_reads_its_lane() {
    let csv = ["csv-bars", "MYFILES", "--venue", "binance", "--symbol", "BTCUSDT"];
    let named =
        import_of(&[&csv[..], &["--interval", "1h", "--lane", "user-my-file"][..]].concat());
    let target = named.target.clone().expect("a target");
    assert_eq!(
        (target.venue.as_str(), target.symbol.as_str(), target.interval.as_str()),
        ("binance", "BTCUSDT", "1h")
    );
    assert_eq!(target.label, "my-file");
    let req = import_request(&named, None, None, import::Mode::Plan);
    assert_eq!(req.target, Some(target));
    assert!(req.bars.is_empty(), "the file's bars are the rows, nothing is derived");

    let missing =
        parse_of(&[&["history", "import"][..], &csv[..], &["--interval", "1h"][..]].concat());
    assert!(missing.unwrap_err().contains("give --symbol, --interval and --lane"));
    let bad =
        [&["history", "import"][..], &csv[..], &["--interval", "1h", "--lane", "user-My File"][..]];
    assert!(parse_of(&bad.concat()).unwrap_err().contains("label"));
    let venue_lane =
        [&["history", "import"][..], &csv[..], &["--interval", "1h", "--lane", "my-file"][..]];
    let e = parse_of(&venue_lane.concat()).unwrap_err();
    assert!(e.contains("not a user's lane") && e.contains("--lane user-my-file"), "{e}");
    // The retired `--label` is refused by name with its replacement, on any verb.
    let retired =
        [&["history", "import"][..], &csv[..], &["--interval", "1h", "--label", "my-file"][..]];
    assert_eq!(
        parse_of(&retired.concat()).unwrap_err(),
        Stop::message(BIN, RetiredDataSpelling::ImportLabel.rendered())
    );
    let e = parse_of(&["history", "import", "dukascopy-bi5", "EURUSD", "--lane", "user-x"])
        .unwrap_err();
    assert!(e.contains("--lane") && e.contains("only csv-bars"), "{e}");
    let e =
        parse_of(&["history", "fetch", "binance:BTCUSDT:1h", "--days", "1", "--lane", "user-x"]);
    assert!(e.unwrap_err().contains("--lane does not apply to `fetch`"));
    for flag in [&["--tz", "UTC"][..], &["--decimal", ","][..]] {
        let argv = [&["history", "import", "dukascopy-bi5", "EURUSD"][..], flag].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(flag[0]) && e.contains("only csv-bars"), "{e}");
        let argv = [&["history", "fetch", "binance:BTCUSDT:1h", "--days", "1"][..], flag].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(flag[0]) && e.contains("import csv-bars"), "{e}");
    }
    assert_eq!(import_of(&["dukascopy-bi5", "EURUSD"]).target, None);

    let lane = vike_data::store::store_kind::user_lane("my-file").unwrap();
    let get = ["history", "get", "binance:BTCUSDT:1h", "--days", "1", "--lane", lane.as_str()];
    assert_eq!(parse_of(&get).unwrap().get.unwrap().source.as_deref(), Some(lane.as_str()));
}

/// `--sparse` (decision 0271's amendment of 2026-11-18) reaches the target, on a dataset and on
/// `--file` alike, and is off without it; it is csv-bars' alone, and so is `--allow-coarse-grid`,
/// refused without `--sparse`.
#[test]
fn csv_bars_sparse_reaches_the_target_and_belongs_to_csv_bars_alone() {
    let csv =
        ["csv-bars", "MYFILES", "--symbol", "S", "--interval", "1m", "--lane", "user-signals"];
    assert!(!import_of(&csv).target.expect("a target").sparse, "dense unless asked");
    assert!(import_of(&[&csv[..], &["--sparse"][..]].concat()).target.expect("a target").sparse);
    let upload = ["csv-bars", "--file", "s.csv", "--symbol", "S", "--interval", "1m"];
    let sent = import_of(&[&upload[..], &["--lane", "user-signals", "--sparse"][..]].concat());
    assert!(sent.target.expect("a target").sparse);
    let e = parse_of(&["history", "import", "dukascopy-bi5", "EURUSD", "--sparse"]).unwrap_err();
    assert!(e.contains("--sparse") && e.contains("only csv-bars"), "{e}");
    let e = parse_of(&["history", "fetch", "binance:BTCUSDT:1h", "--days", "1", "--sparse"]);
    assert!(e.unwrap_err().contains("import csv-bars"));

    // `--allow-coarse-grid` rides beside `--sparse` alone, on a dataset and on `--file`.
    let both = import_of(&[&csv[..], &["--sparse", "--allow-coarse-grid"][..]].concat());
    let target = both.target.expect("a target");
    assert!(target.sparse && target.allow_coarse_grid);
    let alone = [&["history", "import"][..], &csv[..], &["--allow-coarse-grid"][..]].concat();
    let e = parse_of(&alone).unwrap_err();
    assert!(e.contains("--allow-coarse-grid is meaningful only with --sparse"), "{e}");
    let sent =
        [&upload[..], &["--lane", "user-news", "--sparse", "--allow-coarse-grid"][..]].concat();
    assert!(import_of(&sent).target.expect("a target").allow_coarse_grid);
    let e = parse_of(&["history", "import", "dukascopy-bi5", "EURUSD", "--allow-coarse-grid"])
        .unwrap_err();
    assert!(e.contains("--allow-coarse-grid") && e.contains("only csv-bars"), "{e}");
}

/// Decision 0272: `--file` SENDS a file from this machine, so it takes no DATASET and none of an
/// archive's own flags; without `--venue` the series is under the reserved `user` venue, and
/// `--columns` reaches the target as typed, held to the decoder's grammar before a socket opens.
#[test]
fn csv_bars_sends_a_file_under_the_user_venue_and_refuses_an_archives_flags() {
    let upload = [
        "csv-bars",
        "--file",
        "rates.csv",
        "--symbol",
        "USDT-USD",
        "--interval",
        "1d",
        "--lane",
        "user-coinbase",
        "--columns",
        "time=timestamp_ms,close=rate",
    ];
    let sent = import_of(&upload);
    assert_eq!(sent.file.as_deref(), Some(std::path::Path::new("rates.csv")));
    assert!(sent.dataset.is_empty() && sent.bars.is_empty() && !sent.dry_run);
    let target = sent.target.expect("a target");
    assert_eq!((target.venue.as_str(), target.label.as_str()), ("user", "coinbase"));
    assert_eq!(target.columns.as_deref(), Some("time=timestamp_ms,close=rate"));
    assert!(import_of(&[&upload[..], &["--dry-run"][..]].concat()).dry_run);

    let argv = |extra: &[&'static str]| [&["history", "import"][..], &upload[..], extra].concat();
    let e = parse_of(&argv(&["MYFILES"])).unwrap_err();
    assert!(e.contains("takes no DATASET"), "{e}");
    for flag in [&["--from", "2024-01-01"][..], &["--yes"][..], &["--verify"][..]] {
        let e = parse_of(&argv(flag)).unwrap_err();
        assert!(e.contains(flag[0]) && e.contains("does not apply to --file"), "{e}");
    }
    let bad =
        ["history", "import", "csv-bars", "--file", "x.csv", "--symbol", "S", "--interval", "1d"];
    let e = parse_of(
        &[&bad[..], &["--lane", "user-x", "--columns", "time=t,open=o,close=c"][..]].concat(),
    )
    .unwrap_err();
    assert!(e.contains("columns") && e.contains("open"), "{e}");
    for flag in [&["--file", "x.csv"][..], &["--columns", "time=t,close=c"][..]] {
        let argv = [&["history", "import", "dukascopy-bi5", "EURUSD"][..], flag].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(flag[0]) && e.contains("only csv-bars"), "{e}");
        let argv = [&["history", "fetch", "binance:BTCUSDT:1h", "--days", "1"][..], flag].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(flag[0]) && e.contains("import csv-bars"), "{e}");
    }
}

/// The five regional words reach the request as typed, a plain file's left off; and a word the
/// decoder cannot build a format from is a usage error before a socket opens.
#[test]
fn csv_bars_carries_the_files_regional_form_to_the_datahub() {
    let base = [
        "csv-bars",
        "MYFILES",
        "--venue",
        "binance",
        "--symbol",
        "BTCUSDT",
        "--interval",
        "1h",
        "--lane",
        "user-ru",
    ];
    let regional = [
        "--delimiter",
        ";",
        "--decimal",
        ",",
        "--thousands",
        "space",
        "--time-format",
        "DD.MM.YYYY HH:mm",
        "--tz",
        "Europe/Moscow",
    ];
    let named = import_of(&[&base[..], &regional[..]].concat());
    let req = import_request(&named, None, None, import::Mode::Plan);
    let words = req.target.expect("a target").file_format;
    assert_eq!(
        words,
        vike_data::csv_bars::CsvBarsFormatWords {
            delimiter: Some(";".to_string()),
            decimal: Some(",".to_string()),
            thousands: Some("space".to_string()),
            time: Some("DD.MM.YYYY HH:mm".to_string()),
            tz: Some("Europe/Moscow".to_string()),
        }
    );
    vike_data::csv_bars::CsvBarsFormat::parse(&words)
        .expect("the words build the decoder's format");
    let plain = import_of(&base);
    assert!(plain.target.expect("a target").file_format.is_plain());
    for bad in [&["--delimiter", "|"][..], &["--decimal", ";", "--delimiter", ";"][..]] {
        let argv = [&["history", "import"][..], &base[..], bad].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains("format"), "{bad:?}: {e}");
    }
}

/// **A dry run never writes, and only `--verify` decodes without writing**: the request a run sends
/// is decided by its MODE alone, and `--yes` loses to `--dry-run`, as on `rm`.
#[test]
fn a_dry_run_never_writes_and_only_verify_decodes_without_writing() {
    use import::Mode;
    let import_arguments = import_of(&["dukascopy-bi5", "EURUSD"]);
    for (mode, dry_run, verify) in
        [(Mode::Plan, true, false), (Mode::Verify, true, true), (Mode::Import, false, false)]
    {
        let req = import_request(&import_arguments, None, None, mode);
        assert_eq!((req.dry_run, req.verify), (dry_run, verify), "{mode:?}");
        assert_eq!(req.writes(), mode == Mode::Import, "{mode:?}");
    }
    assert_eq!(Mode::of(true, false), Mode::Plan);
    assert_eq!(Mode::of(true, true), Mode::Verify);
    assert_eq!(Mode::of(false, false), Mode::Import);
    let both = import_of(&["dukascopy-bi5", "EURUSD", "--dry-run", "--yes"]);
    assert_eq!(Mode::of(both.dry_run, both.verify), Mode::Plan, "--dry-run wins over --yes");
}

/// `--verify` is a mode of a DRY RUN, and beside an import it is refused rather than read one way
/// or the other — the wire's own rule, made a usage error here.
#[test]
fn verify_without_dry_run_is_refused() {
    let e = parse_of(&["history", "import", "dukascopy-bi5", "EURUSD", "--verify"]).unwrap_err();
    assert!(e.contains("--verify is a mode of --dry-run"), "{e}");
}

/// The two positionals are both required, a third is refused in the verb's own words, and every
/// command line the refusals tell an operator to type parses.
#[test]
fn an_import_needs_a_format_and_a_dataset_and_refuses_a_third() {
    let none = parse_of(&["history", "import"]).unwrap_err();
    assert!(none.contains("import needs a FORMAT and a DATASET"), "{none}");
    let no_dataset = parse_of(&["history", "import", "dukascopy-bi5"]).unwrap_err();
    assert!(no_dataset.contains("import needs a DATASET"), "{no_dataset}");
    assert!(no_dataset.contains("imports/dukascopy-bi5/"), "{no_dataset}");
    let third = parse_of(&["history", "import", "dukascopy-bi5", "EURUSD", "GBPUSD"]).unwrap_err();
    assert!(third.contains("`import` takes TWO") && third.contains("'GBPUSD'"), "{third}");
    for message in [none, no_dataset] {
        let commands: Vec<&str> =
            message.split('`').skip(1).step_by(2).filter(|c| c.starts_with("vike-cli ")).collect();
        assert!(!commands.is_empty(), "the refusal names no command to type: {message}");
        for command in commands {
            let argv: Vec<&str> = command.split_whitespace().collect();
            crate::cmd::accepts(&argv[1..]).unwrap_or_else(|e| panic!("`{command}`: {e}"));
        }
    }
}

/// The DATASET meets the wire's own validator at parse time — the function the client runs before
/// it sends and the server runs at its door — so a name that can never be a dataset is a usage
/// error before a socket opens.
#[test]
fn the_dataset_meets_the_wires_validator_before_a_socket_opens() {
    let long = "A".repeat(33);
    for bad in ["eurusd", "..", "EUR/USD", ".EURUSD", "CON", long.as_str()] {
        let e = parse_of(&["history", "import", "dukascopy-bi5", bad]).unwrap_err();
        assert!(e.contains("import dataset"), "{bad:?}: {e}");
    }
    let e =
        parse_of(&["history", "import", "dukascopy-bi5", "EURUSD", "--bars", "7m"]).unwrap_err();
    assert!(e.contains("--bars") && e.contains("does not divide a UTC day"), "{e}");
}

/// ⚠ **Every flag that is not the import door's own is refused BY NAME there, with a reason true
/// of an import** — never "unknown option", and never another verb's sentence about a listing.
#[test]
fn the_import_door_refuses_every_flag_that_is_not_its_own_by_name() {
    let rows: &[(&[&str], &str)] = &[
        (&["--store", "/srv/hist"], "no local route"),
        (&["--engine", "/opt/backtest"], "no local route"),
        (&["--source", "starter"], "its FORMAT"),
        (&["--days", "7"], "counts back from NOW"),
        (&["--kind", "quote"], "names what it writes exactly"),
        (&["--venue", "dukascopy"], "names what it writes exactly"),
        (&["--name", "EUR"], "names what it writes exactly"),
        (&["--asset-class"], "names what it writes exactly"),
        (&["--partial-only"], "names what it writes exactly"),
        (&["--symbol", "EURUSD"], "two arguments"),
        (&["--group", "G"], "two arguments"),
        (&["--interval", "1m"], "two arguments"),
        (&["--produced-by", "dukascopy:"], "only `rm` deletes"),
        (&["--out", "f.parquet"], "only `export` writes one"),
        (&["--limit", "5"], "ROWS"),
        (&["--window", "1d"], "WINDOWED WALK"),
        (&["--require-days", "3"], "CRITERION"),
    ];
    for &(flag, why) in rows {
        let argv = [&["history", "import", "dukascopy-bi5", "EURUSD"][..], flag].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(flag[0]), "{argv:?} must name {}: {e}", flag[0]);
        assert!(e.contains(why), "{argv:?} must say why ({why}): {e}");
    }
}

/// …and the import door's OWN two flags are refused by name on every other verb, naming the verb
/// that takes them.
#[test]
fn the_import_flags_are_refused_by_name_on_every_other_verb() {
    let mut refused = 0;
    for sub in SUBCOMMANDS.iter().filter(|s| **s != Sub::Import) {
        for flag in [
            &["--bars", "1m"][..],
            &["--verify"][..],
            &["--fetch"][..],
            &["--concurrency", "4"][..],
        ] {
            let argv = [&["history", sub.as_str()][..], flag].concat();
            let e = parse_of(&argv).unwrap_err();
            assert!(e.contains(flag[0]), "{argv:?} must name {}: {e}", flag[0]);
            assert!(e.contains("`vike-cli data history import`"), "{argv:?}: {e}");
            refused += 1;
        }
    }
    assert_eq!(refused, (SUBCOMMANDS.len() - 1) * 4, "every other verb, all four flags");
}

/// **`--fetch` needs a window**, since the bucket bills it; `--concurrency` is its knob alone, and
/// is held to 1..=30.
#[test]
fn fetch_takes_a_named_window_and_a_bounded_concurrency() {
    let base = ["dukascopy-bi5", "EURUSD", "--from", "2024-01-02", "--to", "2024-01-31"];
    assert_eq!(import_of(&[&base[..], &["--fetch"]].concat()).fetch, Some(8));
    let four = import_of(&[&base[..], &["--fetch", "--concurrency", "4"]].concat());
    assert_eq!(four.fetch, Some(4));
    assert_eq!(import_of(&base).fetch, None);
    let e = parse_of(&["history", "import", "dukascopy-bi5", "EURUSD", "--fetch"]).unwrap_err();
    assert!(e.contains("--from and --to"), "{e}");
    for bad in ["0", "31", "many"] {
        let argv = [&["history", "import"][..], &base, &["--fetch", "--concurrency", bad]].concat();
        assert!(parse_of(&argv).unwrap_err().contains("--concurrency"), "{bad}");
    }
    let argv = [&["history", "import"][..], &base, &["--concurrency", "4"]].concat();
    assert!(parse_of(&argv).unwrap_err().contains("a knob of --fetch"));
}

/// Every `vike-cli data history import` line the get-market-data skill tells an agent to run PARSES —
/// read from the RENDERED copy, the one that ships and that an agent executes rather than reads.
#[test]
fn every_import_line_in_the_skill_parses() {
    let skill_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/get-market-data/SKILL.md");
    let skill = std::fs::read_to_string(&skill_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", skill_path.display()));
    let lines: Vec<&str> = skill
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("vike-cli data history import "))
        .collect();
    assert!(lines.len() >= 3, "the skill's import examples were not found: {lines:?}");
    for line in lines {
        let argv: Vec<&str> = line.split_whitespace().collect();
        crate::cmd::accepts(&argv[1..])
            .unwrap_or_else(|e| panic!("the skill tells an agent to run `{line}`: {e}"));
    }
}
