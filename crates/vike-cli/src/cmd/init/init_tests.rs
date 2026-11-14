use super::*;

fn parse_of(args: &[&str]) -> Result<Args, Stop> {
    parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn the_default_is_no_flags_at_all() {
    assert_eq!(
        parse_of(&[]).unwrap(),
        Args { dir: None, reset: false, dry_run: false, strategies_only: false, json: false }
    );
}

/// `--strategies-only` is the installers' call; `--reset` cannot reach the shipped files.
#[test]
fn strategies_only_parses_and_refuses_reset() {
    assert!(parse_of(&["--strategies-only"]).unwrap().strategies_only);
    assert!(parse_of(&["--strategies-only", "--dry-run"]).unwrap().dry_run);
    assert!(parse_of(&["--strategies-only", "--reset"]).unwrap_err().contains("--reset"));
}

#[test]
fn options_parse_in_both_flag_forms() {
    assert_eq!(parse_of(&["--dir", "/tmp/ud"]).unwrap().dir, Some(PathBuf::from("/tmp/ud")));
    assert_eq!(parse_of(&["--dir=/tmp/ud"]).unwrap().dir, Some(PathBuf::from("/tmp/ud")));
    assert!(parse_of(&["--reset"]).unwrap().reset);
    assert!(parse_of(&["--dry-run"]).unwrap().dry_run);
}

#[test]
fn usage_errors_are_clean() {
    assert!(parse_of(&["--nope"]).unwrap_err().contains("--nope"));
    assert!(parse_of(&["--dir"]).unwrap_err().contains("--dir"));
    // A bare boolean must reject an inline value rather than silently reading it as true.
    assert!(parse_of(&["--reset=yes"]).unwrap_err().contains("--reset"));
}

#[test]
fn help_short_circuits() {
    assert!(parse_of(&["--help"]).unwrap_err().is_help());
    assert!(parse_of(&["-h"]).unwrap_err().is_help());
}

/// `--dir` outranks the dispatcher's answer; with neither, this is an ERROR rather than a
/// silent scaffold into the working directory (see [`target`]).
#[test]
fn the_target_is_the_flag_then_the_dispatchers_answer_then_an_error() {
    let resolved = Path::new("/p/user_data");
    let no_flag =
        Args { dir: None, reset: false, dry_run: false, strategies_only: false, json: false };
    assert_eq!(target(&no_flag, Some(resolved)).unwrap(), resolved);

    let with_flag = Args { dir: Some(PathBuf::from("/tmp/x")), ..no_flag };
    assert_eq!(target(&with_flag, Some(resolved)).unwrap(), PathBuf::from("/tmp/x"));
    assert_eq!(target(&with_flag, None).unwrap(), PathBuf::from("/tmp/x"));

    let err = target(&no_flag, None).unwrap_err();
    assert!(err.contains("--dir"), "the error must name the way out: {err}");
}

/// Table paths are `/`-separated because the table is also documentation. On Windows a literal
/// join would produce ONE component containing slashes, and the tree would be a single
/// oddly-named file.
#[test]
fn table_paths_become_real_nested_paths() {
    let got = resolve(Path::new("root"), "presets/sma_cross/fast.toml");
    assert_eq!(got, Path::new("root").join("presets").join("sma_cross").join("fast.toml"));
}

/// Each starter strategy file is a file its own header accepts — and its header declares every
/// knob the shipped profiles hand it, so the run `init` prints is not refused (the integration
/// half, over a real scaffold, is `init_cli.rs`'s `the_runs_init_prints_are_accepted_on_a_fresh_project`).
#[test]
fn every_starter_strategy_file_has_a_header_that_parses() {
    for (rel, body) in strategy_files() {
        if rel.ends_with(".rhai") {
            vike_strategy::file::StrategyFileHeader::parse(&body)
                .unwrap_or_else(|e| panic!("strategies/{rel}'s header does not parse: {e}"));
        }
    }
}

/// ⚠ The map the command PRINTS and the map it WRITES are one string. A user reads the printed
/// tree once, at scaffold time, and the README from then on; if those two could disagree the
/// printed one would be the lie, because nothing ever re-prints it.
#[test]
fn map_is_embedded_verbatim_in_the_readme() {
    assert!(
        content::readme().contains(content::MAP),
        "user_data/README.md must embed the printed MAP verbatim"
    );
}

/// Every folder in the map is a folder the command actually creates, and every folder it
/// creates is in the map. A map naming a folder that never appears is a worse map than none.
#[test]
fn the_map_and_the_created_tree_agree() {
    for dir in DIRS {
        assert!(
            content::MAP.contains(&format!("{dir}/")),
            "{dir}/ is created but absent from the printed map"
        );
    }
    // ...and the other direction, for the folders the map names with their own line.
    for line in content::MAP.lines().skip(1) {
        // `split_whitespace` skips leading whitespace itself, so trimming first is redundant
        // (clippy::trim_split_whitespace). The map indents its rows; this still reads the
        // first token of each.
        let folder = line.split_whitespace().next().unwrap_or_default();
        let folder = folder.trim_end_matches('/');
        assert!(DIRS.contains(&folder), "the map names {folder}/ but DIRS does not create it");
    }
}

/// Each folder ships at least one example — the decision this scaffold exists to implement.
/// `logs/`, `profiles/` and `backtest_results/` are the exceptions and are asserted as such (README
/// only), so removing a README fails here rather than quietly leaving a folder with nothing in it.
#[test]
fn every_folder_ships_content() {
    let shipped = files();
    for dir in DIRS {
        let n = shipped.iter().filter(|(p, _)| p.starts_with(&format!("{dir}/"))).count();
        assert!(n > 0, "{dir}/ ships no file at all");
        if ["logs", "profiles", "backtest_results"].contains(dir) {
            // ⚠ A committed sample log, run profile or result would be fiction: invented
            // timestamps, a slice nobody loaded, numbers nobody measured (the owner's rulings of
            // 2026-11-13: no fake data, no example profiles).
            assert_eq!(n, 1, "{dir}/ must ship its README and nothing else");
            continue;
        }
        assert!(
            shipped
                .iter()
                .any(|(p, _)| p.starts_with(&format!("{dir}/")) && !p.ends_with("README.md")),
            "{dir}/ ships only a README — an empty folder reads as unfinished setup"
        );
    }
}

/// ⚠ **Every command `init` PRINTS is one this binary's real parser accepts** — driven through
/// [`crate::cmd::accepts`], the parser a typed line meets, and never compared with a second copy
/// of the spelling. [`Hint`]'s doc carries the three times their spelling moved, and the one
/// time the lines were left behind — five days of printing three refused commands to every new
/// user while every test here stayed green, because none of them asked a parser.
///
/// The one line that is not `vike-cli` is `vike-backend datahub`, which belongs to ANOTHER
/// binary whose parser this crate cannot link (it is the DataFusion side of the split). It is
/// pinned to exactly those two tokens instead, and
/// `the_printed_datahub_command_names_a_binary_and_verb_this_workspace_builds` checks both
/// against the multicall's own manifest and tool table — a weaker check, declared as one.
#[test]
fn every_printed_command_is_one_the_real_parser_accepts() {
    let mut driven = 0usize;
    for step in next_steps() {
        for hint in &step.run {
            let filled = filled(&hint.argv);
            let argv: Vec<&str> = filled.iter().map(String::as_str).collect();
            match argv.split_first() {
                Some((&"vike-cli", rest)) => {
                    crate::cmd::accepts(rest).unwrap_or_else(|e| {
                        panic!(
                            "`vike-cli init` prints `{}`, which vike-cli REFUSES: {e}",
                            argv.join(" ")
                        )
                    });
                    driven += 1;
                }
                Some((&"vike-backend", rest)) => assert_eq!(
                    rest,
                    ["datahub"],
                    "the one non-vike-cli line init prints is `vike-backend datahub`, and \
                         nothing here can parse another binary's flags — keep it bare"
                ),
                other => {
                    panic!("init prints a command for a program nothing checks: {other:?}")
                }
            }
        }
    }
    // The import, the venue fetch, the starter and the run. A count, so a refactor that stopped
    // yielding hints cannot turn this into a loop over nothing.
    assert_eq!(driven, 4, "the vike-cli lines init prints were not all driven");
}

/// A printed line with each placeholder replaced by a sample value, so the parser judges the
/// SPELLING. The printed text keeps the placeholders: it is an example, never a compiled ticker (the
/// owner's ruling of 2026-11-13).
fn filled(argv: &[String]) -> Vec<String> {
    const SAMPLES: [(&str, &str); 9] = [
        ("<FILE>", "trend/sma_cross.rhai"),
        ("<VENUE>", "v"),
        ("<SYMBOL>", "S"),
        ("<INTERVAL>", "1h"),
        ("<FROM>", "2026-01-01T00"),
        ("<TO>", "2026-02-01T00"),
        ("<CASH>", "10000"),
        ("<FORMAT>", "dukascopy-bi5"),
        ("<DATASET>", "D"),
    ];
    argv.iter()
        .map(|token| SAMPLES.iter().fold(token.clone(), |t, (from, to)| t.replace(from, to)))
        .collect()
}

/// **No profile ships and no data ships, and the printed steps say how to load REAL bars and run a
/// strategy file** (the owner's rulings of 2026-11-13): the table carries no `.toml` under
/// `profiles/` and no file under `backtest_results/` but the READMEs, and the printed lines name the
/// released `data hist import` and a profile-less `backtest run --strategy`, with placeholders and
/// no ticker.
#[test]
fn no_profile_or_data_ships_and_the_hints_say_how_to_load_and_run() {
    for (rel, _) in files() {
        assert!(
            !(rel.starts_with("profiles/") && rel.ends_with(".toml")),
            "{rel}: no profile ships"
        );
        assert!(
            !rel.starts_with("backtest_results/") || rel == "backtest_results/README.md",
            "{rel}: no result ships"
        );
    }
    let lines: Vec<String> =
        next_steps().iter().flat_map(|s| s.run.iter().map(|h| h.argv.join(" "))).collect();
    let all = lines.join("\n");
    assert!(all.contains("vike-cli data hist import <FORMAT> <DATASET>"), "{all}");
    assert!(
        all.contains(
            "vike-cli backtest run --local --strategy <FILE> --venue <VENUE> --symbol \
             <SYMBOL> --interval <INTERVAL> --from <FROM> --to <TO> --cash <CASH>"
        ),
        "{all}"
    );
    assert!(!all.contains("--profile") && !all.contains("demo"), "{all}");
}

/// The KILL PROOF for the test above: the checker refuses each spelling this file has printed
/// and a user then met as a refusal. If [`crate::cmd::accepts`] ever answered `Ok` to these,
/// the test above would pass for a reason that has nothing to do with the spelling.
#[test]
fn the_checker_refuses_every_spelling_init_used_to_print() {
    for retired in [
        &["data", "seed-demo"][..],
        &["data", "fetch-starter"],
        &["data", "fetch", "binance:BTCUSDT:1h", "--days", "180"],
        &["data", "hist", "seed-demo"],
        &["data", "hist", "fetch", "--source", "demo"],
        &["backtest", "--profile", "/p/user_data/profiles/backtest.toml"],
    ] {
        assert!(
            crate::cmd::accepts(retired).is_err(),
            "`vike-cli {}` was accepted, so the parser check above proves nothing",
            retired.join(" ")
        );
    }
}

/// `vike-backend datahub` is the binary a release ships and the verb it answers to. Read from
/// the multicall's own sources rather than restated, because that line cannot be parsed from
/// here: `vike` was that binary's name until v0.1.21, and a stale `vike datahub` is exactly
/// the spelling this pin exists to stop.
#[test]
fn the_printed_datahub_command_names_a_binary_and_verb_this_workspace_builds() {
    let multicall = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vike");
    let manifest = std::fs::read_to_string(multicall.join("Cargo.toml"))
        .expect("crates/vike/Cargo.toml is the multicall binary's manifest");
    assert!(
        manifest.lines().any(|l| l.trim() == "name = \"vike-backend\""),
        "the multicall binary is no longer named `vike-backend`; init prints that name"
    );
    let main = std::fs::read_to_string(multicall.join("src/main.rs"))
        .expect("crates/vike/src/main.rs carries the multicall's tool table");
    assert!(
        main.contains("name: \"datahub\""),
        "the multicall no longer has a `datahub` tool; init tells users to run one"
    );
}

/// Every usage line the shipped PRESETS carry in their own comments is accepted too — each
/// preset's `# Load it with: vike-cli …` (continued with `\`), its placeholders filled. These are files `init` WRITES rather than prints, and the
/// same rot applies: a header that read `vike-cli backtest --profile …` would sit in every
/// scaffold ever made. The READMEs are deliberately NOT scanned — they mention verbs in prose
/// (`vike-cli backtest run`, alone) and redirect output in shell blocks, so reading them as
/// argv would need a shell's judgement this test should not be making.
#[test]
fn every_usage_line_in_a_shipped_profile_or_preset_is_accepted() {
    let mut driven = 0usize;
    for (rel, body) in files() {
        if !rel.ends_with(".toml") {
            continue;
        }
        let mut pending: Option<String> = None;
        for line in body.lines() {
            let Some(text) = line.strip_prefix('#') else {
                pending = None;
                continue;
            };
            let text = text.trim();
            let text = text.strip_prefix("Load it with:").map_or(text, str::trim);
            let command = match pending.take() {
                Some(head) => format!("{head} {text}"),
                None if text.starts_with("vike-cli ") => text.to_string(),
                None => continue,
            };
            if let Some(head) = command.strip_suffix('\\') {
                pending = Some(head.trim_end().to_string());
                continue;
            }
            let tokens: Vec<String> = command.split_whitespace().map(str::to_string).collect();
            let filled = filled(&tokens);
            let argv: Vec<&str> = filled.iter().map(String::as_str).collect();
            crate::cmd::accepts(&argv[1..]).unwrap_or_else(|e| {
                panic!("{rel} tells its reader to run `{command}`, which vike-cli refuses: {e}")
            });
            driven += 1;
        }
    }
    // Four presets, one usage line each (no profile ships).
    assert!(driven >= 4, "only {driven} usage line(s) were found — the scan stopped reading");
}

/// No shipped file may be empty, and none may be listed twice — a duplicate row would make
/// `--reset`'s "restore the sample" ambiguous about which sample.
#[test]
fn the_table_is_well_formed() {
    let shipped = files();
    let mut paths: Vec<&str> = shipped.iter().map(|(p, _)| *p).collect();
    paths.sort_unstable();
    let before = paths.len();
    paths.dedup();
    assert_eq!(before, paths.len(), "a path is listed twice in files()");
    for (path, body) in &shipped {
        assert!(!body.trim().is_empty(), "{path} ships empty content");
    }
}
