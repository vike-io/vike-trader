//! The `[strategy]` table: WHERE a file runs (`.rhai` here, `.rs` through the sandboxed seam), where
//! its header lets it run, the identity a file gives a mount, and WHICH ACCOUNT a mount trades on.

use super::*;

// ---------------------------------------------------------------------------------------------
// WHERE a strategy file runs.
// ---------------------------------------------------------------------------------------------

/// The headline: a `.rhai` file resolves in this process and a `.rs` file through the sandboxed
/// seam, each into a mountable box carrying the file as named and the sha256 of the bytes read.
#[test]
fn a_rhai_file_resolves_here_and_a_rust_file_through_the_sandboxed_seam() {
    let dir = strategies();
    let rhai = file_profile("buy_hold.rhai").expect("parses");
    match rhai.resolve_mount(dir.path(), true).expect("a live `.rhai` file resolves") {
        MountResolvedStrategy::Script { path, sha256, .. } => {
            assert_eq!(path, "buy_hold.rhai");
            assert_eq!(
                sha256,
                vike_hub_protocol::hash::sha256_hex(test_strategy_files::BUY_HOLD.as_bytes())
            );
        }
        MountResolvedStrategy::Sandboxed { .. } => panic!("a `.rhai` file runs in this process"),
    }
    let rust = file_profile("grid.rs").expect("parses");
    match rust.resolve_mount_with(dir.path(), true, &stand_in(&[])).expect("resolves") {
        MountResolvedStrategy::Sandboxed { path, sha256, .. } => {
            assert_eq!(path, "grid.rs");
            assert_eq!(sha256, vike_hub_protocol::hash::sha256_hex(GRID_RS.as_bytes()));
        }
        MountResolvedStrategy::Script { .. } => {
            panic!("a `.rs` file runs in its sandboxed process")
        }
    }
    assert!(rust.resolve_strategy(dir.path(), false).is_err(), "no sandbox in this build");
}

/// In a process started without a builder (this test binary), the daemon's own opener refuses a
/// `.rs` file BY NAME with the reason, after every check before the seam passed.
#[test]
fn a_rust_file_is_refused_by_name_where_no_sandboxed_process_starts() {
    let dir = strategies();
    let err = match file_profile("grid.rs").expect("parses").resolve_mount(dir.path(), false) {
        Err(e) => e,
        Ok(_) => panic!("this build starts no sandboxed process"),
    };
    assert!(err.contains("strategy file grid.rs"), "names the file: {err}");
    assert!(err.contains("sandboxed process"), "{err}");
}

/// **The header says where a file may run, and the daemon refuses the rest at resolve**: `backtest`
/// never (this daemon does not link the simulator; the header's `why` is quoted), `paper` not while
/// the live gate is on, `live` always. The rhai and the rust arms meet the same gate, before either
/// compiles or opens anything.
#[test]
fn a_file_runs_only_where_its_header_lets_it() {
    for live in [false, true] {
        let err = match mount(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"funding_capture.rs\"\n",
            live,
        ) {
            Err(e) => e,
            Ok(_) => panic!("a backtest-only file never mounts here (live = {live})"),
        };
        assert!(err.contains("runs in a backtest only"), "{err}");
        assert!(err.contains("Bar::funding"), "quotes the header's why: {err}");
        assert!(err.contains("does not link the simulator"), "{err}");
    }
    let paper_only =
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"paper_only.rhai\"\n";
    assert!(mount(paper_only, false).is_ok(), "a paper file mounts on the paper daemon");
    let err = match mount(paper_only, true) {
        Err(e) => e,
        Ok(_) => panic!("a paper file never mounts on a live daemon"),
    };
    assert!(err.contains("cannot mount live"), "{err}");
    assert!(err.contains("runs_on = \"paper\""), "names what the header says: {err}");
    assert!(err.contains("runs_on = \"live\""), "spells the fix: {err}");
    for file in ["buy_hold.rhai", "grid.rs"] {
        let text =
            format!("venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"{file}\"\n");
        for live in [false, true] {
            assert!(mount(&text, live).is_ok(), "{file} says live: it mounts (live = {live})");
        }
    }
}

/// **The shipped files that look into the future never mount, here or on a paper daemon**
/// (`strategies/looks_into_the_future/`, decision 0271's amendment): each is refused as a backtest-only
/// file, quoting its own `why`. Reads the shipped folder at RUN time: a tree without it (the public
/// mirror until the shipped set is published there) skips loudly.
#[test]
fn the_shipped_files_that_look_into_the_future_never_mount() {
    let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../strategies/looks_into_the_future");
    let Ok(entries) = std::fs::read_dir(&shipped) else {
        eprintln!("SKIPPED: {} is absent from this tree", shipped.display());
        return;
    };
    let dir = strategies();
    let mut refused = 0;
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).expect("a file name").to_string();
        let text = std::fs::read_to_string(&path).expect("a readable strategy file");
        let header = vike_strategy::file::StrategyFileHeader::parse(&text).expect("its header");
        let why = header.strategy.why.expect("a backtest-only file says why");
        assert!(why.starts_with("looks into the future: "), "{name}: {why}");
        let file = format!("looks_into_the_future/{name}");
        test_strategy_files::write_into(dir.path(), &[(&file, &text)]);
        let profile =
            format!("venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"{file}\"\n");
        for live in [false, true] {
            let err = match ProfileDaemon::from_toml_str(&profile)
                .expect("parses")
                .resolve_mount_with(dir.path(), live, &stand_in(&[]))
            {
                Err(e) => e,
                Ok(_) => panic!("{file} looks into the future and mounted (live = {live})"),
            };
            assert!(err.contains("runs in a backtest only"), "{err}");
            assert!(err.contains(&why), "quotes the header's why: {err}");
        }
        refused += 1;
    }
    assert_eq!(refused, 6, "the six files of {}", shipped.display());
}

/// A file that is not there fails the resolve naming the path it was looked for at: a typo in the
/// profile is that refusal, never a different strategy.
#[test]
fn a_file_that_is_not_in_the_folder_is_refused_naming_it() {
    let err = match mount(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"trend/grud.rs\"\n",
        false,
    ) {
        Err(e) => e,
        Ok(_) => panic!("a missing file must not mount"),
    };
    assert!(err.contains("read strategy file") && err.contains("grud.rs"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// The identity a FILE gives a mount.
// ---------------------------------------------------------------------------------------------

/// **A file is known by its STEM**, and a `[[mounts]]` row's derived id carries it: `{stem}` for a
/// `.rs` file and `rhai-{stem}` for a `.rhai` file — the spellings a registry row and a script row
/// derived before strategies were files, so a row re-stored to name the file of the strategy it ran
/// keeps its id, its state sidecar and its attribution key.
#[test]
fn a_file_gives_its_mount_the_id_its_registry_name_gave() {
    let p = ProfileDaemon::from_toml_str(
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\n\
         [mounts.strategy]\nfile = \"market_making/spread_maker.rs\"\n\
         [[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\n\
         [mounts.strategy]\nfile = \"buy_hold.rhai\"\n\
         [[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\naccount = \"ALT\"\n\
         [mounts.strategy]\nfile = \"grid_dca/grid.rs\"\n",
    )
    .expect("three distinct rows");
    let rows = p.mount_rows();
    let named: Vec<(&str, String)> =
        rows.iter().map(|r| (r.strategy_name(), r.derived_controller_id())).collect();
    assert_eq!(
        named,
        vec![
            ("spread_maker", "bybit__BTCUSDT__1m__spread_maker".to_string()),
            ("buy_hold", "bybit__BTCUSDT__1m__rhai-buy_hold".to_string()),
            ("grid", "bybit__BTCUSDT__1m__grid__ALT".to_string()),
        ]
    );
    // The declared cost: two files sharing a stem on one series and one account derive ONE id,
    // and the pair is refused at load naming both rows.
    let err = ProfileDaemon::from_toml_str(
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\n[mounts.strategy]\nfile = \"a/grid.rs\"\n\
         [[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\n[mounts.strategy]\nfile = \"b/grid.rs\"\n",
    )
    .expect_err("one id for two rows");
    assert!(err.contains("mounts[0] and mounts[1]") && err.contains("SAME mount id"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// WHICH ACCOUNT a mount trades on: the three states a profile can state.
// ---------------------------------------------------------------------------------------------

/// **A profile can NAME the unlabelled account (`account = "DEFAULT"`), on both spellings.**
///
/// The three states are: absent (no account named: ambiguous once the venue runs two engines, which
/// `vike_core`'s `mount_engine_resolution` refuses for that ONE mount), a label (that account) and
/// `DEFAULT` (the unlabelled account, never ambiguous). `AccountLabel`'s own serde refuses `DEFAULT`
/// on purpose, so before `wire_account_option` was put on these two fields the refusal's own advice
/// ("name `DEFAULT`") was unsatisfiable from a profile and the unlabelled account of a two-account
/// venue had NO spelling here.
#[test]
fn a_profile_can_name_the_unlabelled_account_a_label_or_nothing() {
    use vike_model::accounts::account_keys::AccountLabel;
    let single = |account: &str| {
        ProfileDaemon::from_toml_str(&format!(
            "venue = \"bybit\"\nsymbol = \"BTCUSDT\"\n{account}{NOTHING_TABLE}"
        ))
        .expect("parses")
    };
    assert_eq!(single("").account, None, "absent names no account");
    assert_eq!(single("account = \"DEFAULT\"").account, Some(AccountLabel::Default));
    assert_eq!(
        single("account = \"LIVE\"").account,
        Some(AccountLabel::parse("LIVE").expect("legal"))
    );
    let multi = ProfileDaemon::from_toml_str(&format!(
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\naccount = \"DEFAULT\"\n{NOTHING_ROW_TABLE}\
         [[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\naccount = \"LIVE\"\n{NOTHING_ROW_TABLE}"
    ))
    .expect("a [[mounts]] row can name DEFAULT beside a labelled one");
    let accounts: Vec<_> = multi.mounts.iter().map(|m| m.account.clone()).collect();
    assert_eq!(
        accounts,
        vec![Some(AccountLabel::Default), Some(AccountLabel::parse("LIVE").expect("legal"))]
    );
    // The reserved word stays out of the LABEL space: `DEFAULT` is not a way to spell a label.
    assert!(AccountLabel::parse("DEFAULT").is_err());
}
