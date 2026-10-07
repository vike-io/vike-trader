//! Pins each bridge mount's `PROBE_SYMBOL` against the [`WIRED_MARKETS`] row for the same venue.
//!
//! # Why this exists
//!
//! Five bridges scope the startup credential probe's reconcile client to ONE hard-coded symbol, a
//! private `const PROBE_SYMBOL: &str` in `crates/bridges/<venue>/src/mount.rs`, and each constant's
//! doc says it mirrors the venue's `WIRED_MARKETS` row. A doc comment mirrors nothing: the row
//! lives in this crate and the constant in a bridge that cannot name it (the bridge sits below the
//! daemon, and the constant is private besides), so retargeting the wired market left the probe
//! scoped to the old symbol with every test green. The probe's balance read is account-wide, which
//! is why that failure is quiet — the symbol only scopes the client's order and position reads.
//!
//! This is the missing half, built the way `live_wired_venues_pin.rs`
//! reads `feeds.rs`: a TEXT gate over the bridge sources, claiming nothing beyond what text can
//! show. Two directions:
//!
//!   - every gated bridge's constant equals the symbol of its wired row, and
//!   - every bridge mount that declares a `PROBE_SYMBOL` at all is gated, so a sixth one cannot
//!     appear unpinned.
//!
//! [`the_probe_symbol_scanner_can_actually_fail`] is the mutation self-test: the scan is pure over
//! the source text, so it is fed a drifted constant, a commented-out one and a missing one, and
//! must report each. An unmutated pin is the defect this file exists to repair.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_tradehub::wired_markets::WIRED_MARKETS;

/// The bridges whose mount scopes its probe to a `PROBE_SYMBOL`, named by the directory under
/// `crates/bridges/` — which is also the venue id and so the `WIRED_MARKETS` row's key. Every one
/// of these rows is unconditional (no feature gates it), so the pin runs in every build.
const GATED: [&str; 5] = ["alpaca", "binance", "bybit", "ctrader", "okx"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn mount_rs(bridge: &str) -> String {
    format!("crates/bridges/{bridge}/src/mount.rs")
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read `{rel}`: {e}"))
}

/// The literal a `const PROBE_SYMBOL: &str = "…";` declares in `source`, or `None`.
///
/// Whole-line `//` comments are skipped first — the constants are documented with prose that quotes
/// symbols, and a commented-out declaration must not satisfy the pin. The declaration is matched
/// by its head (`const PROBE_SYMBOL: &str = "`), so a visibility prefix does not hide it.
fn probe_symbol_in(source: &str) -> Option<String> {
    const HEAD: &str = "const PROBE_SYMBOL: &str = \"";
    source.lines().filter(|l| !l.trim_start().starts_with("//")).find_map(|line| {
        let after = &line[line.find(HEAD)? + HEAD.len()..];
        after.split_once('"').map(|(literal, _)| literal.to_string())
    })
}

/// Why `source`'s probe symbol disagrees with `wired`, or `None` when it agrees. Pure over the
/// text, so [`the_probe_symbol_scanner_can_actually_fail`] can feed it known cases.
fn drift(bridge: &str, source: &str, wired: &str) -> Option<String> {
    match probe_symbol_in(source) {
        None => Some(format!(
            "{bridge}: `{}` declares no `const PROBE_SYMBOL: &str = \"…\";` — the constant moved \
             or was renamed. Re-anchor this pin on whatever scopes the probe now; do NOT delete \
             it, or the doc that says the two agree is a doc again.",
            mount_rs(bridge)
        )),
        Some(probe) if probe != wired => Some(format!(
            "{bridge}: `{}`'s PROBE_SYMBOL is {probe:?} but its WIRED_MARKETS row mounts \
             {wired:?}. The probe's reconcile client is scoped to a symbol the daemon no longer \
             mounts. Move the two together.",
            mount_rs(bridge)
        )),
        Some(_) => None,
    }
}

fn wired_symbol(bridge: &str) -> &'static str {
    WIRED_MARKETS
        .iter()
        .find(|m| m.venue == bridge)
        .unwrap_or_else(|| panic!("{bridge} has a PROBE_SYMBOL but no WIRED_MARKETS row"))
        .symbol
}

/// Every gated bridge's `PROBE_SYMBOL` is its wired row's symbol.
#[test]
fn a_bridges_probe_symbol_is_its_wired_markets_symbol() {
    let drifted: Vec<String> = GATED
        .iter()
        .filter_map(|bridge| drift(bridge, &read(&mount_rs(bridge)), wired_symbol(bridge)))
        .collect();
    assert!(
        drifted.is_empty(),
        "\nPROBE_SYMBOL drifted from WIRED_MARKETS:\n\n{}\n",
        drifted.join("\n")
    );
}

/// The converse: a bridge mount that declares a `PROBE_SYMBOL` and is not in [`GATED`] would be an
/// unpinned copy of a wired symbol, which is the defect again.
#[test]
fn every_bridge_that_declares_a_probe_symbol_is_gated() {
    let bridges = workspace_root().join("crates").join("bridges");
    let declaring: BTreeSet<String> = std::fs::read_dir(&bridges)
        .unwrap_or_else(|e| panic!("cannot list `{}`: {e}", bridges.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let dir = entry.file_name().to_string_lossy().into_owned();
            let source = std::fs::read_to_string(entry.path().join("src").join("mount.rs")).ok()?;
            probe_symbol_in(&source).map(|_| dir)
        })
        .collect();
    let gated: BTreeSet<String> = GATED.iter().map(|b| (*b).to_string()).collect();
    assert_eq!(
        declaring, gated,
        "the bridges that declare a PROBE_SYMBOL differ from the ones this pin gates — add the new \
         bridge to GATED (and give it a WIRED_MARKETS row), or delete the stale entry"
    );
}

/// The mutation self-test: the scan reports a drifted constant, a commented-out one and a missing
/// one, and passes the matching case — so a green run of the two tests above means the constants
/// were read, not that the scanner returned nothing.
#[test]
fn the_probe_symbol_scanner_can_actually_fail() {
    let matching = "/// doc\nconst PROBE_SYMBOL: &str = \"BTCUSDT\";\n";
    assert_eq!(drift("planted", matching, "BTCUSDT"), None, "the control must pass");

    let drifted = drift("planted", matching, "ETHUSDT").expect("a drifted constant is reported");
    assert!(drifted.contains("\"BTCUSDT\"") && drifted.contains("\"ETHUSDT\""), "{drifted}");

    let commented = "// const PROBE_SYMBOL: &str = \"BTCUSDT\";\n";
    assert!(
        drift("planted", commented, "BTCUSDT").is_some(),
        "a commented-out declaration must not satisfy the pin"
    );

    assert!(
        drift("planted", "fn nothing() {}\n", "BTCUSDT").is_some(),
        "a missing one is reported"
    );

    let prefixed = "pub(crate) const PROBE_SYMBOL: &str = \"EURUSD\";\n";
    assert_eq!(probe_symbol_in(prefixed).as_deref(), Some("EURUSD"), "a visibility prefix is read");
}
