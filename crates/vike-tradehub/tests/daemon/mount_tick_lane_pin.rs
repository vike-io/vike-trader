//! Pins that EVERY mounted feed arm wires a tick lane beside its bar lane (decision 0245): a mount works by
//! candles AND by ticks, and there is no bar-only mount.
//!
//! # Why this exists
//!
//! A mount whose arm subscribed bars and nothing else would have an engine whose price board is written at
//! each bar close and never between (the core hands a quote, trade and book to the engine; with none arriving
//! the 30-second risk clock skips the engine as stale and judges it once per closed bar). Every arm in
//! `crates/vike-tradehub/src/feeds/` wires both today; this gate keeps it so. Like
//! `cex_feed_wiring_pin.rs` it is a TEXT gate over the arms' source (`live_runtime` spawns a core and opens
//! sockets, so no test can call the arms against a real dial), and it claims only what text can show.
//!
//! # What is pinned
//!
//! * the ROSTER: the venues in `FEED_ARMS` are exactly the venues of [`MOUNT_LANES`], so a new venue arm
//!   cannot arrive without a row here;
//! * per venue, the arm's own function body contains a BAR lane (`subscribe_bars(`; polymarket's and
//!   ctrader's bars are SYNTHESIZED from the price lane by a `MakerSink`, said by a [`BarLane::Synthesized`]
//!   row that also demands that sink) and at least one TICK-class lane (`subscribe_quotes(`,
//!   `subscribe_trades(`, `subscribe_book(` or `subscribe_ticks(`).
//!
//! [`the_scanner_can_actually_fail`] is the mutation self-test: a bars-only copy of every arm is REJECTED.

use std::path::{Path, PathBuf};

/// How a venue's closed bars reach the core.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BarLane {
    /// The arm subscribes the venue's own bar lane.
    Native,
    /// No bar lane exists at the venue: a `MakerSink` folds the price lane into closed bars.
    Synthesized,
}

/// `(venue, file under feeds/, the top-level functions that make up the mounted arm, bar lane)`.
type Row = (&'static str, &'static str, &'static [&'static str], BarLane);

/// THE ROSTER of mounted arms. One row per `FEED_ARMS` venue; the CEX venues share one arm.
const MOUNT_LANES: &[Row] = &[
    ("alpaca", "alpaca.rs", &["fn wire_alpaca_feed("], BarLane::Native),
    ("aster", "cex.rs", &["fn wire_cex_kline_lane(", "fn spawn_cex_tick_pump("], BarLane::Native),
    ("binance", "cex.rs", &["fn wire_cex_kline_lane(", "fn spawn_cex_tick_pump("], BarLane::Native),
    ("bybit", "cex.rs", &["fn wire_cex_kline_lane(", "fn spawn_cex_tick_pump("], BarLane::Native),
    ("ctrader", "ctrader.rs", &["fn wire_ctrader_feed("], BarLane::Synthesized),
    ("deribit", "deribit.rs", &["fn wire_deribit_feed("], BarLane::Native),
    ("hyperliquid", "hyperliquid.rs", &["fn wire_hyperliquid_feed("], BarLane::Native),
    ("ig", "ig.rs", &["fn wire_ig_feed("], BarLane::Native),
    (
        "oanda",
        "oanda.rs",
        &["fn wire_oanda_feed(", "fn subscribe_quotes_surviving("],
        BarLane::Native,
    ),
    ("okx", "cex.rs", &["fn wire_cex_kline_lane(", "fn spawn_cex_tick_pump("], BarLane::Native),
    ("polymarket", "polymarket.rs", &["fn wire_polymarket_feeds("], BarLane::Synthesized),
];

/// The tick-class verbs: a quote, a trade print, a book or the CEX tick pump.
const TICK_VERBS: [&str; 4] =
    ["subscribe_quotes(", "subscribe_trades(", "subscribe_book(", "subscribe_ticks("];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn feeds_file(name: &str) -> String {
    let path = workspace_root().join("crates/vike-tradehub/src/feeds").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The CODE of one top-level function, from its signature to the closing brace at column 0, comments
/// stripped (a `//` mention of a verb cannot spoof the check).
fn body(src: &str, anchor: &str) -> String {
    assert_eq!(
        src.matches(anchor).count(),
        1,
        "the anchor {anchor:?} must match its file EXACTLY once, or this gate reads a slice nobody chose"
    );
    let start = src.find(anchor).expect("checked above");
    let end = src[start..].find("\n}\n").map_or(src.len(), |e| start + e);
    src[start..end]
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The predicate: a bar lane (or its synthesizer) AND a tick-class lane in the arm's code.
fn wires_bars_and_ticks(code: &str, bars: BarLane) -> Result<(), String> {
    match bars {
        BarLane::Native if !code.contains("subscribe_bars(") => {
            return Err("no `subscribe_bars(`: the arm wires no bar lane".into());
        }
        BarLane::Synthesized if !code.contains("MakerSink") => {
            return Err(
                "declared synthesized, but the arm builds no `MakerSink` to fold bars".into()
            );
        }
        _ => {}
    }
    if TICK_VERBS.iter().any(|v| code.contains(v)) {
        Ok(())
    } else {
        Err(format!(
            "no tick-class lane ({TICK_VERBS:?}): a mount on this arm would be bar-only, its engine's price \
             board written at bar closes and never between"
        ))
    }
}

fn arm_code(row: &Row) -> String {
    let src = feeds_file(row.1);
    row.2.iter().map(|a| body(&src, a)).collect::<Vec<_>>().join("\n")
}

/// The venues of `FEED_ARMS`, read off `feeds.rs`: the rows `("venue", venue_arm),`.
fn feed_arm_venues() -> Vec<String> {
    let src = std::fs::read_to_string(workspace_root().join("crates/vike-tradehub/src/feeds.rs"))
        .expect("feeds.rs");
    let start = src.find("pub(crate) const FEED_ARMS").expect("FEED_ARMS");
    let end = src[start..].find("\n];").map_or(src.len(), |e| start + e);
    src[start..end]
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("(\"")?;
            let (venue, tail) = rest.split_once('"')?;
            tail.trim_start().starts_with(", ").then(|| venue.to_string())
        })
        .collect()
}

/// The roster is complete both ways: a new `FEED_ARMS` venue needs a row, and a row needs a venue.
#[test]
fn the_roster_of_mounted_arms_is_the_feed_arms() {
    let mut table: Vec<&str> = MOUNT_LANES.iter().map(|r| r.0).collect();
    table.sort_unstable();
    let mut arms = feed_arm_venues();
    arms.sort_unstable();
    assert_eq!(
        arms, table,
        "FEED_ARMS and MOUNT_LANES name different venues: a mounted arm needs a row here (and a tick lane)"
    );
}

/// Every mounted arm wires a bar lane and a tick-class lane. A mount works by candles AND by ticks.
#[test]
fn every_mounted_arm_wires_bars_and_a_tick_lane() {
    for row in MOUNT_LANES {
        if let Err(why) = wires_bars_and_ticks(&arm_code(row), row.3) {
            panic!("{}: {why}", row.0);
        }
    }
}

/// **The mutation self-test**: strip the tick lanes from each arm's code and the predicate must REJECT it,
/// then strip the bar lane and it must reject that too. An unmutated pin proves nothing.
#[test]
fn the_scanner_can_actually_fail() {
    for row in MOUNT_LANES {
        let code = arm_code(row);
        assert!(wires_bars_and_ticks(&code, row.3).is_ok(), "{}: the real arm must pass", row.0);
        let mut bars_only = code.clone();
        for verb in TICK_VERBS {
            bars_only = bars_only.replace(verb, "noop(");
        }
        assert_ne!(bars_only, code, "{}: the tick-lane mutation must change the text", row.0);
        assert!(
            wires_bars_and_ticks(&bars_only, row.3).is_err(),
            "{}: a bars-only copy of the arm must be REJECTED",
            row.0
        );
        let ticks_only = match row.3 {
            BarLane::Native => code.replace("subscribe_bars(", "noop("),
            BarLane::Synthesized => code.replace("MakerSink", "Sink"),
        };
        assert_ne!(ticks_only, code, "{}: the bar-lane mutation must change the text", row.0);
        assert!(
            wires_bars_and_ticks(&ticks_only, row.3).is_err(),
            "{}: a copy without its bar lane must be REJECTED",
            row.0
        );
    }
}
