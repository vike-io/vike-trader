//! **What each strategy FILE mounts**: for every profile shape below, through
//! `ProfileDaemon::resolve_mount_with` (the function `main` calls, with the stand-in of the sandboxed
//! process for a `.rs` file), the arm it resolves through, the file as the profile named it, the
//! sha256 of the bytes read, the name the mount is known by, the effective-params line, and — for a
//! `.rs` file — the mount facts the sandboxed process is told when it opens.
//!
//! It replaces the golden of the profile shapes the A-S maker mounted (captured before step 10): no
//! maker is compiled into the program any more, so what a maker profile mounts is its FILE's answer
//! to the same facts, proven where that file lives
//! (`crates/vike-strategies-check/tests/makers/maker_golden.txt`, read by that crate's
//! `tests/makers/facts.rs`).

use std::sync::Mutex;

use super::*;

/// The arm a file resolves through.
#[derive(Debug, PartialEq)]
enum Arm {
    /// In this process (`MountResolvedStrategy::Script`).
    Here,
    /// In its sandboxed process (`MountResolvedStrategy::Sandboxed`).
    Sandboxed,
}

/// What one row of a profile mounted.
#[derive(Debug, PartialEq)]
struct Mounted {
    arm: Arm,
    path: String,
    sha256: String,
    name: String,
    params: String,
}

/// The facts a `.rs` file's sandboxed process was opened with: venue, symbol, interval, grid, the
/// static fee lane's maker round trip, and whether the bytes it was handed are the bytes hashed.
#[derive(Debug, PartialEq)]
struct Facts {
    venue: String,
    symbol: String,
    interval: String,
    tick_size: f64,
    maker_round_trip_fee: Option<f64>,
    source_is_the_hashed_bytes: bool,
}

/// Every row of `text` resolved over [`strategies`], `.rs` rows echoing `echo`; the facts each `.rs`
/// row was opened with, in row order.
fn mounted(
    text: &str,
    echo: &'static [(&'static str, &'static str)],
) -> (Vec<Mounted>, Vec<Facts>) {
    let dir = strategies();
    let facts = Mutex::new(Vec::new());
    let opener = |request: &SandboxOpen<'_>| {
        facts.lock().expect("one test thread").push(Facts {
            venue: request.mount.venue.clone(),
            symbol: request.mount.symbol.clone(),
            interval: request.mount.interval.clone(),
            tick_size: request.mount.tick_size,
            maker_round_trip_fee: request.maker_round_trip_fee,
            source_is_the_hashed_bytes: vike_hub_protocol::hash::sha256_hex(
                request.source.as_bytes(),
            ) == request.sha256,
        });
        stand_in(echo)(request)
    };
    let profile = ProfileDaemon::from_toml_str(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let rows = profile
        .mount_rows()
        .iter()
        .map(|row| {
            let name = row.strategy_name().to_string();
            match row
                .resolve_mount_with(dir.path(), false, &opener)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
            {
                MountResolvedStrategy::Script { path, sha256, params, .. } => {
                    Mounted { arm: Arm::Here, path, sha256, name, params }
                }
                MountResolvedStrategy::Sandboxed { path, sha256, params, .. } => {
                    Mounted { arm: Arm::Sandboxed, path, sha256, name, params }
                }
            }
        })
        .collect();
    (rows, facts.into_inner().expect("one test thread"))
}

fn sha(text: &str) -> String {
    vike_hub_protocol::hash::sha256_hex(text.as_bytes())
}

/// A `.rhai` file mounts in this process; its line is the file plus the overrides that landed.
#[test]
fn a_rhai_file_mounts_here_under_its_own_name_and_hash() {
    let (rows, facts) = mounted(
        "token_id = \"TOK\"\n[strategy]\nfile = \"buy_hold.rhai\"\n[strategy.params]\nsize = 3.0\n",
        &[],
    );
    assert_eq!(
        rows,
        vec![Mounted {
            arm: Arm::Here,
            path: "buy_hold.rhai".into(),
            sha256: sha(test_strategy_files::BUY_HOLD),
            name: "buy_hold".into(),
            params: "script=buy_hold.rhai size=3".into(),
        }]
    );
    assert!(facts.is_empty(), "a `.rhai` file opens no sandboxed process");
}

/// A `.rs` file mounts through its sandboxed process, told the MOUNT's facts: the profile's own grid
/// (a stated one, or the venue's default) and the static fee lane's maker round trip, the bytes it is
/// handed being the bytes hashed. Its line is its own echo.
#[test]
fn a_rust_file_mounts_through_its_sandboxed_process_told_the_mount_facts() {
    for (text, venue, symbol, tick_size) in [
        (
            "venue = \"bybit\"\nsymbol = \"BTCUSDT\"\ntick_size = 0.1\n[strategy]\n\
             file = \"market_making/spread_maker.rs\"\n",
            "bybit",
            "BTCUSDT",
            0.1,
        ),
        (
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"market_making/spread_maker.rs\"\n",
            "hyperliquid",
            "BTC",
            1.0,
        ),
        (
            "token_id = \"TOK\"\ninterval = \"5m\"\ninterval_ms = 300000\n[strategy]\n\
             file = \"market_making/spread_maker.rs\"\n",
            "polymarket",
            "TOK",
            0.01,
        ),
    ] {
        let (rows, facts) = mounted(text, &[("qty", "1")]);
        assert_eq!(
            rows,
            vec![Mounted {
                arm: Arm::Sandboxed,
                path: "market_making/spread_maker.rs".into(),
                sha256: sha(SPREAD_MAKER_RS),
                name: "spread_maker".into(),
                params: "qty=1".into(),
            }],
            "{venue}"
        );
        assert_eq!(
            facts,
            vec![Facts {
                venue: venue.into(),
                symbol: symbol.into(),
                interval: if venue == "polymarket" { "5m" } else { "1m" }.into(),
                tick_size,
                maker_round_trip_fee: vike_tradehub_core::maker_round_trip_fee_for(venue, symbol),
                source_is_the_hashed_bytes: true,
            }],
            "{venue}"
        );
    }
}

/// A `[[mounts]]` profile mixing both kinds mounts each row through its own arm, under its own name.
#[test]
fn a_mixed_mounts_profile_mounts_each_file_through_its_own_arm() {
    let (rows, facts) = mounted(
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\ntick_size = 0.1\nseed_cash = 5000.0\n\
         [mounts.strategy]\nfile = \"market_making/spread_maker.rs\"\n[mounts.strategy.params]\nqty = 0.002\n\
         [[mounts]]\ntoken_id = \"TOK\"\n[mounts.strategy]\nfile = \"buy_hold.rhai\"\n\
         [[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\naccount = \"ALT\"\ninterval = \"5m\"\n\
         interval_ms = 300000\n[mounts.strategy]\nfile = \"grid.rs\"\n[mounts.strategy.params]\n\
         rungs = 2\nsize = 1.0\nstep = 10.0\n",
        &[],
    );
    let arms: Vec<(&Arm, &str, &str)> =
        rows.iter().map(|m| (&m.arm, m.path.as_str(), m.name.as_str())).collect();
    assert_eq!(
        arms,
        vec![
            (&Arm::Sandboxed, "market_making/spread_maker.rs", "spread_maker"),
            (&Arm::Here, "buy_hold.rhai", "buy_hold"),
            (&Arm::Sandboxed, "grid.rs", "grid"),
        ]
    );
    let opened: Vec<(&str, &str)> =
        facts.iter().map(|f| (f.venue.as_str(), f.interval.as_str())).collect();
    assert_eq!(opened, vec![("bybit", "1m"), ("bybit", "5m")], "one open per `.rs` row");
}
