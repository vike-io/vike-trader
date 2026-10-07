use vike_ui_theme::icons;
use vike_ui_theme::maps;

use super::WinKind;

#[test]
fn every_kind_round_trips_through_its_slug() {
    for k in WinKind::ALL {
        assert_eq!(WinKind::from_slug(k.slug()), Some(k), "slug {:?}", k.slug());
    }
    // Distinct slugs — `from_slug`'s first-match resolution must be unambiguous.
    let mut slugs: Vec<&str> = WinKind::ALL.iter().map(|k| k.slug()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(slugs.len(), WinKind::ALL.len(), "duplicate slug in WinKind::ALL");
}

#[test]
fn unknown_and_miscased_slugs_parse_to_none() {
    // Case-sensitive, like the `VIKE_TOOL` match this replaced — the caller owns the fallback.
    assert_eq!(WinKind::from_slug(""), None);
    assert_eq!(WinKind::from_slug("News"), None);
    assert_eq!(WinKind::from_slug("data manager"), None); // the LABEL is not the slug
    assert_eq!(WinKind::from_slug("nope"), None);
}

#[test]
fn the_trade_window_and_the_account_window_have_their_own_slugs() {
    assert_eq!(WinKind::from_slug("trade"), Some(WinKind::Trade));
    assert_eq!(WinKind::from_slug("account"), Some(WinKind::Account));
    assert_eq!(WinKind::from_slug("dom"), None, "the DOM is gone; no alias");
    // `WinKind::Connections` is gone the same way, deleted outright by
    // Connections-merges-into-Data-Manager (not merely renamed or aliased) — so `"connections"`
    // retires exactly like `"dom"` did: `VIKE_TOOL=connections` falls back to Calendar via the
    // caller's `.unwrap_or(Calendar)` rather than resolving to anything. Deliberate, not an
    // oversight: unlike the Trade ticket's Connect click (one call site, unambiguously meaning
    // "go to Credentials"), this QA env hook has no comparable single intent to seed, and
    // `VIKE_SHOT_WIN=connections` already covers the "capture the Credentials screen" QA need
    // under its own, still-working spelling (`crate::ui::startup`'s capture harness).
    assert_eq!(WinKind::from_slug("connections"), None, "the standalone window is gone; no alias");
    assert!(WinKind::Trade.carries_title_tabs(), "the view controls sit in the title bar");
    assert!(!WinKind::Account.carries_title_tabs());
}

/// The legacy `VIKE_TOOL=<slug>` grid: every value the replaced match named still resolves to
/// the same kind, with `.unwrap_or(Calendar)` supplying the old `_ =>` fallback for junk.
///
/// The `("dom", WinKind::Dom)` row is GONE, deliberately: `WinKind::Dom` was removed with the DOM
/// window (one name per thing, no alias), so `"dom"` now parses to `None` and falls back to
/// `Calendar` like any other junk slug —
/// `the_trade_window_and_the_account_window_have_their_own_slugs` pins that. `"trade"` is the
/// vocabulary's one slug whose WINDOW changed (the new Trade window; the old one is `"account"`).
#[test]
fn legacy_vike_tool_vocabulary_is_unchanged() {
    for (s, want) in [
        ("trade", WinKind::Trade),
        ("options", WinKind::Options),
        ("greeks", WinKind::Greeks),
        ("tearsheet", WinKind::Tearsheet),
        ("news", WinKind::News),
        ("data", WinKind::Data),
        ("polymarket", WinKind::Polymarket),
        ("calendar", WinKind::Calendar),
    ] {
        assert_eq!(WinKind::from_slug(s).unwrap_or(WinKind::Calendar), want, "slug {s:?}");
    }
    assert_eq!(WinKind::from_slug("junk").unwrap_or(WinKind::Calendar), WinKind::Calendar);
}

/// The title and the glyph each kind wears, pinned to what `WinKind::label` and `WinKind::icon` answered
/// before they moved to `ui-theme.toml`'s `window_kind` map (the Trade window's glyph is the ladder, the
/// Data window's title is "Data Manager"). Changing a row of the table changes a window's title bar, and
/// this test is what says so by name; it also holds that every kind is listed, so a new kind is not left
/// out of the pin.
#[test]
fn a_window_kind_wears_the_title_and_the_glyph_the_window_kind_map_gives_it() {
    let pinned = [
        (WinKind::Chart, "Chart", icons::CHART),
        (WinKind::Trade, "Trade", icons::DOM),
        (WinKind::Account, "Account", icons::ACCOUNT),
        (WinKind::Options, "Options", icons::OPTIONS),
        (WinKind::Greeks, "Greeks", icons::GREEKS),
        (WinKind::News, "News", icons::NEWS),
        (WinKind::Calendar, "Calendar", icons::CALENDAR),
        (WinKind::Data, "Data Manager", icons::DATA),
        (WinKind::Studio, "Studio", icons::STUDIO),
        (WinKind::Tearsheet, "Tearsheet", icons::TEARSHEET),
        (WinKind::Polymarket, "Polymarket", icons::POLYMARKET),
        (WinKind::Settings, "Settings", icons::SETTINGS),
    ];
    assert_eq!(pinned.len(), WinKind::ALL.len(), "a kind is missing from the pin");
    for (kind, title, glyph) in pinned {
        assert_eq!(kind.label(), title, "{kind:?}: the title");
        assert_eq!(kind.icon(), glyph, "{kind:?}: the glyph");
    }
}

/// The map is the windows' own: every kind reads the row of its own name, no two share one, and no row of
/// the `window_kind` map is left without a kind.
#[test]
fn every_kind_has_its_own_window_kind_row_and_every_row_its_kind() {
    for k in WinKind::ALL {
        assert_eq!(k.row().key, format!("{k:?}").to_uppercase(), "{k:?} reads another row");
        assert_eq!(
            WinKind::ALL.iter().filter(|o| o.row() == k.row()).count(),
            1,
            "{k:?}'s row is shared"
        );
    }
    for row in maps::window_kind::ALL {
        assert!(WinKind::ALL.iter().any(|k| k.row() == *row), "{} is no window kind", row.key);
    }
}

/// A kind's title and glyph are both written in its row: `WinKind::label` and `WinKind::icon` read them and
/// have no answer for a row that lacks one.
#[test]
fn every_window_kind_row_carries_a_title_and_a_glyph() {
    for row in maps::window_kind::ALL {
        assert!(row.word.is_some_and(|w| !w.is_empty()), "{}: no title", row.key);
        assert!(row.icon().is_some(), "{}: no glyph of the registry", row.key);
    }
}
