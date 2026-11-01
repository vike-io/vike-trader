//! The TRADE module's boundary, as text: `src/trade/**` is written with no dependency on the wire
//! adapter or on anything that owns a socket (decision
//! `docs/decisions/0165-vike-mount-splits-and-no-other-hub-gets-a-core-crate.md`, verdict 1; the plan's
//! R35), and names no bridge, no `vike-tradehub-core` and no `vike-core`. The one exception is the venue
//! half, `src/trade/adapters/**` (W9b): it names `vike-bridge-core` and the bridges, never the fold.
//!
//! The scan reads the source tree at run time and ignores comment lines, so the module docs may name the
//! path they refuse. A gate that read nothing would pass for ever, so the scan checks it found the lane.
#![cfg(feature = "trade")]

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every non-comment line of `src/trade/**` with its file and line number.
fn code_lines() -> Vec<(String, usize, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("trade");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut out = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("a readable source file");
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            out.push((file.display().to_string(), i + 1, line.to_string()));
        }
    }
    out
}

#[test]
fn the_scan_finds_the_lane() {
    let lines = code_lines();
    assert!(
        lines.iter().any(|(_, _, l)| l.contains("pub trait TradeLane")),
        "the scan did not find `trait TradeLane` under src/trade: it reads nothing"
    );
}

#[test]
fn trade_names_no_server_path() {
    let bad: Vec<_> = code_lines()
        .into_iter()
        .filter(|(_, _, l)| l.contains("crate::server") || l.contains("super::server"))
        .collect();
    assert!(
        bad.is_empty(),
        "src/trade/** must not name crate::server (the wire adapter is server/trade.rs): {bad:?}"
    );
}

#[test]
fn trade_names_no_bridge_and_no_core() {
    const FORBIDDEN: &[&str] = &[
        "vike_bridge_core",
        "vike_tradehub_core",
        "vike_tradehub",
        "vike_core::",
        "vike_binance",
        "vike_bybit",
        "vike_okx",
        "vike_aster",
        "vike_hyperliquid",
        "vike_polymarket",
        "vike_deribit",
        "vike_dukascopy",
        "vike_fxcm",
        "vike_oanda",
        "vike_ibkr",
        "vike_ig",
        "vike_ctrader",
        "vike_alpaca",
    ];
    // The venue half (`src/trade/adapters/**`, moved in from `vike-tradehub-core` by W9b) IS where the
    // bridges are named: the sessions around it must not be.
    let bad: Vec<_> = code_lines()
        .into_iter()
        .filter(|(file, _, _)| !is_venue_half(file))
        .filter(|(_, _, l)| FORBIDDEN.iter().any(|f| l.contains(f)))
        .collect();
    assert!(
        bad.is_empty(),
        "src/trade/** outside adapters/ names a crate it must not link: {bad:?}"
    );
}

/// Whether a source file is part of the venue half (`src/trade/adapters.rs` and `src/trade/adapters/**`).
fn is_venue_half(file: &str) -> bool {
    let file = file.replace('\\', "/");
    file.ends_with("/trade/adapters.rs") || file.contains("/trade/adapters/")
}

/// The venue half names the bridges and `vike-bridge-core`, and nothing above or beside the daemon: not
/// the tradehub's fold (`vike_tradehub_core`, which the gatewayhub's closure may not hold), not
/// `vike-core`, not the other daemon. Test modules are exempt only where they are `#[cfg(test)]`
/// children, whose dev-dependency on the fold is how the roster tests drive it.
#[test]
fn the_venue_half_names_no_fold_and_no_other_daemon() {
    const FORBIDDEN: &[&str] =
        &["vike_tradehub", "vike_core::", "vike_datahub", "vike_paper", "vike_sim"];
    let bad: Vec<_> = code_lines()
        .into_iter()
        .filter(|(file, _, _)| is_venue_half(file))
        .filter(|(file, _, _)| !is_test_file(file))
        .filter(|(_, _, l)| FORBIDDEN.iter().any(|f| l.contains(f)))
        .collect();
    assert!(
        bad.is_empty(),
        "the venue half's production code names a crate it must not link: {bad:?}"
    );
}

/// A `*_tests.rs`, `*_tests/`, `testutil` file of the venue half: unit tests that reach the fold through
/// the dev-dependency.
fn is_test_file(file: &str) -> bool {
    let file = file.replace('\\', "/");
    let name = file.rsplit('/').next().unwrap_or("");
    name.ends_with("_tests.rs")
        || name == "testutil.rs"
        || file.contains("_tests/")
        || file.contains("/testutil/")
}

#[test]
fn the_venue_half_is_found() {
    assert!(
        code_lines()
            .iter()
            .any(|(f, _, l)| is_venue_half(f) && l.contains("pub struct BridgeTrade")),
        "the scan did not find `BridgeTrade` under src/trade/adapters: it reads nothing there"
    );
}
