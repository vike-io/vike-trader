//! `import`'s grammar: a FORMAT and a DATASET, inclusive-day bounds, and its flag refusals.

use super::*;

// ── `import`: a vendor archive, read on the datahub's own box ─────────────────────────────────

/// Midnight UTC of `y-m-d`, in epoch-ms.
fn utc_midnight(y: i64, m: u32, d: u32) -> i64 {
    vike_model::time::days_from_civil(y, m, d) * vike_model::MS_PER_DAY
}

fn import_of(args: &[&str]) -> ImportArgs {
    let mut argv = vec!["hist", "import"];
    argv.extend_from_slice(args);
    parse_of(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}")).import.expect("an ImportArgs")
}

/// **The import door's grammar**: a FORMAT and a DATASET, the window defaulting to the whole
/// dataset and the bars to `1m` — and nothing of it left in the fields the other verbs read.
#[test]
fn import_takes_a_format_and_a_dataset_and_defaults_the_rest() {
    let a = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD"]).unwrap();
    assert_eq!(a.sub, Sub::Import);
    assert_eq!(a.addr, DEFAULT_ADDR, "it dials the datahub default");
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
        }
    );
    let a = parse_of(&[
        "hist",
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
    assert!(a.json && a.addr_given && a.addr == "127.0.0.1:9");
    let im = a.import.expect("an ImportArgs");
    assert_eq!(im.bars, vec!["1m".to_string(), "1h".to_string()]);
    assert!(im.dry_run && im.verify && im.yes);
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
        let mut argv = vec!["hist", "import", "dukascopy-bi5", "EURUSD"];
        argv.extend_from_slice(args);
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(needle), "{args:?} must say {needle:?}: {e}");
    }
}

/// **A dry run never writes, and only `--verify` decodes without writing**: the request a run sends
/// is decided by its MODE alone, and `--yes` loses to `--dry-run`, as on `rm`.
#[test]
fn a_dry_run_never_writes_and_only_verify_decodes_without_writing() {
    use import::Mode;
    let im = import_of(&["dukascopy-bi5", "EURUSD"]);
    for (mode, dry_run, verify) in
        [(Mode::Plan, true, false), (Mode::Verify, true, true), (Mode::Import, false, false)]
    {
        let req = import_request(&im, None, None, mode);
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
    let e = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD", "--verify"]).unwrap_err();
    assert!(e.contains("--verify is a mode of --dry-run"), "{e}");
}

/// The two positionals are both required, a third is refused in the verb's own words, and every
/// command line the refusals tell an operator to type parses.
#[test]
fn an_import_needs_a_format_and_a_dataset_and_refuses_a_third() {
    let none = parse_of(&["hist", "import"]).unwrap_err();
    assert!(none.contains("import needs a FORMAT and a DATASET"), "{none}");
    let no_dataset = parse_of(&["hist", "import", "dukascopy-bi5"]).unwrap_err();
    assert!(no_dataset.contains("import needs a DATASET"), "{no_dataset}");
    assert!(no_dataset.contains("imports/dukascopy-bi5/"), "{no_dataset}");
    let third = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD", "GBPUSD"]).unwrap_err();
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
        let e = parse_of(&["hist", "import", "dukascopy-bi5", bad]).unwrap_err();
        assert!(e.contains("import dataset"), "{bad:?}: {e}");
    }
    let e = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD", "--bars", "7m"]).unwrap_err();
    assert!(e.contains("--bars") && e.contains("does not divide a UTC day"), "{e}");
}

/// ⚠ **Every flag that is not the import door's own is refused BY NAME there, with a reason true
/// of an import** — never "unknown option", and never another verb's sentence about a listing.
#[test]
fn the_import_door_refuses_every_flag_that_is_not_its_own_by_name() {
    let rows: &[(&[&str], &str)] = &[
        (&["--store", "/srv/hist"], "no local route"),
        (&["--engine", "/opt/backtest"], "no local route"),
        (&["--source", "demo"], "its FORMAT"),
        (&["--days", "7"], "counts back from NOW"),
        (&["--kind", "quote"], "names what it writes exactly"),
        (&["--venue", "dukascopy"], "names what it writes exactly"),
        (&["--name", "EUR"], "names what it writes exactly"),
        (&["--class"], "names what it writes exactly"),
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
        let argv = [&["hist", "import", "dukascopy-bi5", "EURUSD"][..], flag].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(flag[0]), "{argv:?} must name {}: {e}", flag[0]);
        assert!(e.contains(why), "{argv:?} must say why ({why}): {e}");
        assert!(!e.contains("unknown option"), "{argv:?} must not read as a typo: {e}");
    }
}

/// …and the import door's OWN two flags are refused by name on every other verb, naming the verb
/// that takes them.
#[test]
fn the_import_flags_are_refused_by_name_on_every_other_verb() {
    let mut refused = 0;
    for sub in SUBCOMMANDS.iter().filter(|s| **s != Sub::Import) {
        for flag in [&["--bars", "1m"][..], &["--verify"][..]] {
            let argv = [&["hist", sub.as_str()][..], flag].concat();
            let e = parse_of(&argv).unwrap_err();
            assert!(e.contains(flag[0]), "{argv:?} must name {}: {e}", flag[0]);
            assert!(e.contains("`vike-cli data hist import`"), "{argv:?}: {e}");
            refused += 1;
        }
    }
    assert_eq!(refused, (SUBCOMMANDS.len() - 1) * 2, "every other verb, both flags");
}

/// Every `vike-cli data hist import` line the get-market-data skill tells an agent to run PARSES —
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
        .filter(|l| l.starts_with("vike-cli data hist import "))
        .collect();
    assert!(lines.len() >= 3, "the skill's import examples were not found: {lines:?}");
    for line in lines {
        let argv: Vec<&str> = line.split_whitespace().collect();
        crate::cmd::accepts(&argv[1..])
            .unwrap_or_else(|e| panic!("the skill tells an agent to run `{line}`: {e}"));
    }
}
