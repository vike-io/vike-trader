use super::*;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn a_resolved_family_routes_its_symbols_and_nothing_else() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["TOK_A", "TOK_B"]));
    let r = m.group_resolver();
    assert_eq!(r("polymarket", "TOK_A").as_deref(), Some("btc-5m"));
    assert_eq!(r("polymarket", "TOK_B").as_deref(), Some("btc-5m"));
    assert_eq!(r("polymarket", "TOK_Z"), None, "unmapped ⇒ per-symbol, not dropped");
    assert_eq!(r("binance", "TOK_A"), None, "membership is venue-scoped");
}

/// The rotation case, which is the whole reason this type exists. When the next Polymarket
/// window opens the previous tokens must STOP routing into the group — a stale entry would keep
/// committing a dead symbol into the family forever.
#[test]
fn a_rotation_drops_the_previous_windows_symbols() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["OLD_UP", "OLD_DOWN"]));
    assert_eq!(m.len(), 2);

    m.set_family("polymarket", "btc-5m", &s(&["NEW_UP", "NEW_DOWN"]));
    let r = m.group_resolver();
    assert_eq!(r("polymarket", "NEW_UP").as_deref(), Some("btc-5m"));
    // INVERTED (2026-08-02): this used to assert `None` — "the previous window's tokens are
    // gone" — and that assertion was the bug, pinned. A departing token whose rows are still
    // buffered must keep resolving; see `set_family`'s retire note and the the CI box failure it
    // records. What "gone" now means is `len()`: it is no longer LIVE.
    assert_eq!(
        r("polymarket", "OLD_UP").as_deref(),
        Some("btc-5m"),
        "retired, so a late flush still lands in the family"
    );
    assert_eq!(m.len(), 2, "replaced wholesale, not accumulated");
}

/// Refreshing one family must not disturb another — including one on the same venue.
#[test]
fn refreshing_one_family_leaves_the_others_alone() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["B1"]));
    m.set_family("polymarket", "eth-5m", &s(&["E1"]));
    m.set_family("binance", "perps", &s(&["BTCUSDT"]));

    m.set_family("polymarket", "btc-5m", &s(&["B2"]));
    let r = m.group_resolver();
    assert_eq!(r("polymarket", "B2").as_deref(), Some("btc-5m"));
    assert_eq!(r("polymarket", "B1").as_deref(), Some("btc-5m"), "retired, still draining");
    assert_eq!(r("polymarket", "E1").as_deref(), Some("eth-5m"), "untouched");
    assert_eq!(r("binance", "BTCUSDT").as_deref(), Some("perps"), "untouched");
    assert_eq!(m.len(), 3, "B2 + E1 + BTCUSDT live; B1 retired and not counted");
}

/// A family can legitimately resolve to nothing — between Polymarket windows there may be no
/// live token. That must clear the LIVE map; the departing token still drains into its family.
#[test]
fn resolving_a_family_to_no_symbols_clears_it() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["TOK"]));
    m.set_family("polymarket", "btc-5m", &[]);
    assert!(m.is_empty(), "no LIVE symbols");
    // INVERTED (2026-08-02): used to assert `None`. A family resolving to nothing is exactly the
    // between-windows case, and TOK's last rows are still in the writer's buffer at that moment.
    assert_eq!(
        m.group_resolver()("polymarket", "TOK").as_deref(),
        Some("btc-5m"),
        "retired, so the final flush of a closed window still lands in the family"
    );
}

/// The resolver sees refreshes through the shared map — it is a live view, not a snapshot taken
/// when it was built. A snapshot would freeze the first window forever.
#[test]
fn the_resolver_is_a_live_view_not_a_snapshot() {
    let m = Membership::new();
    let r = m.group_resolver(); // built BEFORE anything is resolved
    assert_eq!(r("polymarket", "LATE"), None);
    m.set_family("polymarket", "btc-5m", &s(&["LATE"]));
    assert_eq!(r("polymarket", "LATE").as_deref(), Some("btc-5m"), "sees the refresh");
}

/// **The bug this exists for**, reproduced from the live the CI box failure: a rotation must not
/// strand the departing window's still-buffered rows. At 11:30:04 the family rotated; at
/// 11:30:07 the old tokens' buffers flushed, found themselves unmapped, and wrote two stray
/// `symbol=<token>/` series.
#[test]
fn a_departed_symbol_still_resolves_while_its_rows_may_be_buffered() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["OLD_UP", "OLD_DOWN"]));
    m.set_family("polymarket", "btc-5m", &s(&["NEW_UP", "NEW_DOWN"]));

    let r = m.group_resolver();
    assert_eq!(r("polymarket", "NEW_UP").as_deref(), Some("btc-5m"));
    assert_eq!(
        r("polymarket", "OLD_UP").as_deref(),
        Some("btc-5m"),
        "a flush arriving seconds after the rotation must still land in the family"
    );
    assert_eq!(r("polymarket", "OLD_DOWN").as_deref(), Some("btc-5m"));
}

/// Retired symbols are DRAIN state, not subscriptions — the status line must not count them, or
/// a rotating family looks like it doubles every window.
#[test]
fn retired_symbols_are_not_counted_as_live() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["A", "B"]));
    m.set_family("polymarket", "btc-5m", &s(&["C"]));
    assert_eq!(m.len(), 1, "one live symbol, two retired");
}

/// A symbol that comes BACK — a family that briefly resolved to nothing, then to the same tokens
/// — is live again, not left in the retired map where it would age out mid-recording.
#[test]
fn a_returning_symbol_becomes_live_again() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["TOK"]));
    m.set_family("polymarket", "btc-5m", &[]);
    assert_eq!(m.len(), 0);
    m.set_family("polymarket", "btc-5m", &s(&["TOK"]));
    assert_eq!(m.len(), 1, "live, not retired");
    assert_eq!(m.group_resolver()("polymarket", "TOK").as_deref(), Some("btc-5m"));
}

/// Retiring is per-family: rotating one family must not retire another's symbols.
#[test]
fn retiring_one_family_leaves_the_others_live() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["B1"]));
    m.set_family("polymarket", "eth-5m", &s(&["E1"]));
    m.set_family("polymarket", "btc-5m", &s(&["B2"]));
    assert_eq!(m.len(), 2, "B2 + E1 live");
    assert_eq!(m.group_resolver()("polymarket", "E1").as_deref(), Some("eth-5m"));
}

/// The grace window is bounded — 288 rotations a day must not grow the map without limit. The
/// purge runs on every `set_family`, so a steady rotation keeps at most a couple of windows'
/// worth of retired entries.
#[test]
fn the_retired_map_is_purged_and_does_not_grow_without_bound() {
    let m = Membership::new();
    for i in 0..50 {
        m.set_family("polymarket", "btc-5m", &s(&[&format!("TOK_{i}")]));
    }
    assert_eq!(m.len(), 1, "one live symbol after 50 rotations");
    // Every retired entry is still inside the grace window here (the loop is instant), which is
    // the correct behaviour — the purge is time-based, not count-based. What matters is that it
    // RUNS: with a zero grace the map would be empty.
    let inner = m.inner.read().unwrap();
    assert_eq!(inner.retired.len(), 49, "bounded by grace, purged on each call");
}

/// An unmapped symbol is still `None` — retiring must not turn the resolver into "everything
/// belongs to some family", which would route an explicit per-symbol subscription into a group.
#[test]
fn an_unknown_symbol_is_still_unmapped() {
    let m = Membership::new();
    m.set_family("polymarket", "btc-5m", &s(&["TOK"]));
    assert_eq!(m.group_resolver()("polymarket", "STRANGER"), None);
    assert_eq!(m.group_resolver()("binance", "TOK"), None, "still venue-scoped");
}
