use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn the_default_is_no_flags_at_all() {
    assert_eq!(
        parse_of(&[]).unwrap(),
        Args { dir: None, reset: false, dry_run: false, json: false }
    );
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
    assert!(parse_of(&["--nope"]).unwrap_err().contains("unknown option"));
    assert!(parse_of(&["--dir"]).unwrap_err().contains("requires a value"));
    // A bare boolean must reject an inline value rather than silently reading it as true.
    assert!(parse_of(&["--reset=yes"]).unwrap_err().contains("takes no value"));
}

#[test]
fn help_short_circuits() {
    assert_eq!(parse_of(&["--help"]).unwrap_err(), "help requested");
    assert_eq!(parse_of(&["-h"]).unwrap_err(), "help requested");
}

/// `--dir` outranks the dispatcher's answer; with neither, this is an ERROR rather than a
/// silent scaffold into the working directory (see [`target`]).
#[test]
fn the_target_is_the_flag_then_the_dispatchers_answer_then_an_error() {
    let resolved = Path::new("/p/user_data");
    let no_flag = Args { dir: None, reset: false, dry_run: false, json: false };
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
    let got = resolve(Path::new("root"), "strategies/rhai/sma_cross/sma_cross.rhai");
    assert_eq!(
        got,
        Path::new("root").join("strategies").join("rhai").join("sma_cross").join("sma_cross.rhai")
    );
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
/// `logs/` is the ONE exception and is asserted as such, so removing its README fails here
/// rather than quietly leaving a folder with nothing in it.
#[test]
fn every_folder_ships_content() {
    let shipped = files();
    for dir in DIRS {
        if *dir == "strategies" {
            continue; // a pure parent: its two children carry the examples
        }
        let n = shipped.iter().filter(|(p, _)| p.starts_with(&format!("{dir}/"))).count();
        assert!(n > 0, "{dir}/ ships no file at all");
        if *dir == "logs" {
            // ⚠ A committed sample log would be invented timestamps describing a run that
            // never happened, and compile.log is rewritten on the next start regardless.
            assert_eq!(n, 1, "logs/ must ship its README and nothing else");
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
    let root = Path::new("/p/user_data");
    let mut driven = 0usize;
    for step in next_steps(root) {
        for hint in &step.run {
            let argv: Vec<&str> = hint.argv.iter().map(String::as_str).collect();
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
    // The seed, the starter, the run and the venue fetch. A count, so a refactor that stopped
    // yielding hints cannot turn this into a loop over nothing.
    assert_eq!(driven, 4, "the vike-cli lines init prints were not all driven");
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

/// The seeding command the shipped demo profile names is the one [`next_steps`] prints, and
/// the parser accepts it. The profile is a raw TOML literal, so it holds a COPY — this is what
/// ties that copy to the value the parser was asked about.
#[test]
fn the_demo_profile_names_the_seeding_command_init_prints() {
    let argv: Vec<&str> = content::SEED_DEMO.split_whitespace().collect();
    assert_eq!(argv.first(), Some(&"vike-cli"));
    crate::cmd::accepts(&argv[1..])
        .unwrap_or_else(|e| panic!("`{}` is refused: {e}", content::SEED_DEMO));
    assert!(
        content::PROFILE_BACKTEST.contains(&format!("`{}`", content::SEED_DEMO)),
        "the shipped profile no longer names `{}` — a reader who hits an empty run has \
             nothing to follow",
        content::SEED_DEMO
    );
}

/// Every usage line the shipped PROFILES and PRESETS carry in their own comments is accepted
/// too — the `#   vike-cli …` header of each profile (continued with `\`) and each preset's
/// `# Load it with: vike-cli …`. These are files `init` WRITES rather than prints, and the
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
            let argv: Vec<&str> = command.split_whitespace().collect();
            crate::cmd::accepts(&argv[1..]).unwrap_or_else(|e| {
                panic!("{rel} tells its reader to run `{command}`, which vike-cli refuses: {e}")
            });
            driven += 1;
        }
    }
    // Three profiles and four presets, one usage line each.
    assert!(driven >= 7, "only {driven} usage line(s) were found — the scan stopped reading");
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
