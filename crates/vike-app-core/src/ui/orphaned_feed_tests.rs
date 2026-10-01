use super::{orphaned_feed_keys, workspace};
use std::collections::HashSet;

fn chart_win(symbol: &str, interval: &str) -> workspace::WinState {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
    workspace::WinState::new("t", symbol, interval, workspace::WinKind::Chart, r)
}

fn spawned(keys: &[&str]) -> HashSet<String> {
    keys.iter().map(|s| s.to_string()).collect()
}

#[test]
fn empty_inputs_reap_nothing() {
    assert!(orphaned_feed_keys(&[], &HashSet::new()).is_empty());
    let wins = vec![chart_win("BTCUSDT", "1m")];
    assert!(orphaned_feed_keys(&wins, &HashSet::new()).is_empty());
}

#[test]
fn a_primary_symbol_with_no_window_left_is_orphaned() {
    let wins = vec![chart_win("BTCUSDT", "1m")];
    let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m"]);
    assert_eq!(orphaned_feed_keys(&wins, &sp), vec!["ETHUSDT@1m".to_string()]);
}

#[test]
fn a_compare_symbol_still_referenced_by_another_window_is_not_reaped() {
    let mut a = chart_win("BTCUSDT", "1m");
    a.compare.push("ETHUSDT".to_string());
    let b = chart_win("ETHUSDT", "1m"); // ETHUSDT is ALSO window b's own primary
    let wins = vec![a, b];
    let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m"]);
    assert!(orphaned_feed_keys(&wins, &sp).is_empty());
}

/// The scenario the fix targets: a Compare symbol backing a live feed loses its only
/// referencing window (chip removed, or FIX 1's primary-switch cascade) — mutating the
/// SAME `WinState` in place between assertions (it isn't `Clone`) stands in for "before"
/// and "after" a `remove_compare` call.
#[test]
fn removing_the_only_reference_orphans_the_compare_feed() {
    let mut a = chart_win("BTCUSDT", "1m");
    a.compare.push("ETHUSDT".to_string());
    let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m"]);
    assert!(
        orphaned_feed_keys(std::slice::from_ref(&a), &sp).is_empty(),
        "still referenced by a's compare list"
    );

    a.compare.clear(); // simulates WinState::remove_compare
    assert_eq!(
        orphaned_feed_keys(std::slice::from_ref(&a), &sp),
        vec!["ETHUSDT@1m".to_string()],
        "no window references ETHUSDT@1m anymore"
    );
    // the window's OWN primary feed is never reaped by this
    assert!(!orphaned_feed_keys(std::slice::from_ref(&a), &sp).contains(&"BTCUSDT@1m".to_string()));
}

/// A foreign-source study (TradingView "symbol" input) keeps its source symbol's feed alive
/// exactly like a Compare overlay does — the feed is subscribed per-frame for the study, so it
/// must not be reaped while the study exists; clearing the source (or removing the study) drops
/// the reference and lets the feed be reaped normally. Cross-venue: the key is `series_key`d, so
/// a non-Binance source is namespaced (never collides with a same-symbol Binance feed).
#[test]
fn a_foreign_source_study_symbol_is_not_reaped_and_is_venue_keyed() {
    use vike_chart::indicators::SourceSymbol;
    let mut a = chart_win("BTCUSDT", "1m");
    a.add_indicator("rsi", &[]); // oscillator on this chart
    a.indicators[0].source_symbol =
        Some(SourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() });
    // the bybit-namespaced source feed AND the window's own primary are both live.
    let sp = spawned(&["BTCUSDT@1m", "bybit:ETHUSDT@1m"]);
    assert!(
        orphaned_feed_keys(std::slice::from_ref(&a), &sp).is_empty(),
        "a study's foreign-source feed must count as a live consumer"
    );
    // clearing the source orphans exactly that feed (the primary is untouched).
    a.indicators[0].source_symbol = None;
    assert_eq!(
        orphaned_feed_keys(std::slice::from_ref(&a), &sp),
        vec!["bybit:ETHUSDT@1m".to_string()],
        "clearing the source releases only the source feed"
    );
}

/// A closed (`open=false`, "hidden off-desktop, rail can unhide") or minimized window still
/// counts as live — mirrors how nothing in `main.rs` ever tears down a closed window's OWN
/// primary feed either; a compare feed must not be treated more aggressively.
#[test]
fn a_closed_or_minimized_window_still_counts_as_live() {
    let mut w = chart_win("BTCUSDT", "1m");
    let sp = spawned(&["BTCUSDT@1m"]);

    w.open = false;
    assert!(orphaned_feed_keys(std::slice::from_ref(&w), &sp).is_empty(), "closed ≠ gone");

    w.open = true;
    w.minimized = true;
    assert!(orphaned_feed_keys(std::slice::from_ref(&w), &sp).is_empty(), "minimized ≠ gone");
}

/// The shared raw-trade-tape key (`"SYM@trades"`, feeding Tick/Volume `aggs` AND SP2
/// orderflow `of_aggs`, per-SYMBOL not per-window) must NEVER be reaped by this diff — it
/// isn't part of the per-window live-set formula at all, so a naive diff would treat every
/// one as orphaned on every call. This is the precise trap the fix's doc calls out.
#[test]
fn trade_tape_keys_are_never_reaped() {
    let wins = vec![chart_win("BTCUSDT", "1m")];
    let sp = spawned(&["BTCUSDT@1m", "BTCUSDT@trades", "ETHUSDT@trades"]);
    assert_eq!(orphaned_feed_keys(&wins, &sp), Vec::<String>::new());
}

/// Tool windows OTHER than DOM (Trade/Options/News/Calendar/Data) never contribute to the
/// live set — and must not interfere with reaping a real orphaned chart key either. (DOM is
/// the one exception — see the tests below; it drives a real `ensure_feed`/`spawned` entry
/// for its own symbol.)
#[test]
fn non_dom_tool_windows_never_contribute_to_the_live_set() {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
    let trade = workspace::WinState::tool("t", workspace::WinKind::Trade, r);
    let wins = vec![trade];
    let sp = spawned(&["BTCUSDT@1m"]);
    assert_eq!(orphaned_feed_keys(&wins, &sp), vec!["BTCUSDT@1m".to_string()]);
}

fn dom_win(symbol: &str) -> workspace::WinState {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
    let mut w = workspace::WinState::tool("d", workspace::WinKind::Dom, r);
    w.symbol = symbol.to_string(); // mirrors `ws.symbol = "BTCUSDT".to_string()` at DOM open
    w
}

/// DOM review finding: a DOM window's OWN fixed `"1m"` bar feed (ensured once, at window
/// creation, NOT re-asserted per frame the way a Chart window's compare list is) must be
/// protected — this is the scenario that would otherwise regress a DOM's mark/PAPER-fill
/// feed the very next frame after opening, with no path to recover it (`ensure_depth`'s own
/// `dom_depth` idempotency gate never notices the underlying bar subscription died).
#[test]
fn a_dom_windows_own_1m_feed_is_protected_even_with_no_matching_chart() {
    let wins = vec![dom_win("BTCUSDT")];
    let sp = spawned(&["BTCUSDT@1m"]);
    assert!(orphaned_feed_keys(&wins, &sp).is_empty(), "DOM's own feed must survive");
}

/// A DOM window only protects its OWN symbol at the fixed `"1m"` — a DIFFERENT symbol's
/// Kline feed (no Chart or DOM referencing it) is still correctly reaped alongside it, and
/// DOM never protects a non-`"1m"` interval on its own symbol either (DOM has no interval
/// of its own to widen the match with — `WinState::interval` stays empty for it).
#[test]
fn a_dom_window_does_not_protect_unrelated_keys() {
    let wins = vec![dom_win("BTCUSDT")];
    let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m", "BTCUSDT@5m"]);
    let mut orphaned = orphaned_feed_keys(&wins, &sp);
    orphaned.sort();
    assert_eq!(orphaned, vec!["BTCUSDT@5m".to_string(), "ETHUSDT@1m".to_string()]);
}
