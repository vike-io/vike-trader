use vike_ui_theme::icons;
use vike_ui_theme::maps;

use super::WindowKind;

#[test]
fn every_kind_round_trips_through_its_slug() {
    for k in WindowKind::ALL {
        assert_eq!(WindowKind::from_slug(k.slug()), Some(k), "slug {:?}", k.slug());
    }
    // Distinct slugs — `from_slug`'s first-match resolution must be unambiguous.
    let mut slugs: Vec<&str> = WindowKind::ALL.iter().map(|k| k.slug()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(slugs.len(), WindowKind::ALL.len(), "duplicate slug in WindowKind::ALL");
}

#[test]
fn unknown_and_miscased_slugs_parse_to_none() {
    // Case-sensitive, like the `VIKE_TOOL` match this replaced — the caller owns the fallback.
    assert_eq!(WindowKind::from_slug(""), None);
    assert_eq!(WindowKind::from_slug("News"), None);
    assert_eq!(WindowKind::from_slug("data manager"), None); // the LABEL is not the slug
    assert_eq!(WindowKind::from_slug("nope"), None);
}

#[test]
fn the_trade_window_and_the_account_window_have_their_own_slugs() {
    assert_eq!(WindowKind::from_slug("trade"), Some(WindowKind::Trade));
    assert_eq!(WindowKind::from_slug("account"), Some(WindowKind::Account));
    assert_eq!(WindowKind::from_slug("dom"), None, "the DOM is gone; no alias");
    // `WindowKind::Connections` is gone the same way, deleted outright by
    // Connections-merges-into-Data-Manager (not merely renamed or aliased) — so `"connections"`
    // retires exactly like `"dom"` did: `VIKE_TOOL=connections` falls back to Calendar via the
    // caller's `.unwrap_or(Calendar)` rather than resolving to anything. Deliberate, not an
    // oversight: unlike the Trade ticket's Connect click (one call site, unambiguously meaning
    // "go to Gateway"), this QA env hook has no comparable single intent to seed, and
    // `VIKE_SHOT_WIN=gateway-compact` already covers the "capture the Gateway screen" QA need
    // under its own, still-working spelling (`crate::ui::startup`'s capture harness).
    assert_eq!(
        WindowKind::from_slug("connections"),
        None,
        "the standalone window is gone; no alias"
    );
    // The Greeks window is the Options window's Portfolio Greeks tab since 2026-11-17: its launcher
    // opens that tab (`crate::ui::window::spawn::WindowSpawnTarget::PortfolioGreeks`), its QA slug retires.
    assert_eq!(WindowKind::from_slug("greeks"), None, "the Greeks window is a tab; no alias");
    // The Execution-history ("Tearsheet") window is the Account window's History tab since top layers
    // wave 3: its slug retires, and `VIKE_TOOL=tearsheet` is refused (`plan_initial_arrange`).
    assert_eq!(WindowKind::from_slug("tearsheet"), None, "the Tearsheet is a tab; no alias");
    assert!(WindowKind::Trade.carries_title_tabs(), "the view controls sit in the title bar");
    assert!(!WindowKind::Account.carries_title_tabs());
}

/// The legacy `VIKE_TOOL=<slug>` grid: every value the replaced match named still resolves to
/// the same kind, with `.unwrap_or(Calendar)` supplying the old `_ =>` fallback for junk.
///
/// The `("dom", WindowKind::Dom)` and `("greeks", WindowKind::Greeks)` rows are GONE, deliberately: each
/// kind was removed with its window (one name per thing, no alias), so `"dom"` and `"greeks"` now
/// parse to `None` and fall back to `Calendar` like any other junk slug —
/// `the_trade_window_and_the_account_window_have_their_own_slugs` pins that. `"trade"` is the
/// vocabulary's one slug whose WINDOW changed (the new Trade window; the old one is `"account"`).
#[test]
fn legacy_vike_tool_vocabulary_is_unchanged() {
    for (s, want) in [
        ("trade", WindowKind::Trade),
        ("options", WindowKind::Options),
        ("news", WindowKind::News),
        ("data", WindowKind::Data),
        ("polymarket", WindowKind::Polymarket),
        ("calendar", WindowKind::Calendar),
    ] {
        assert_eq!(WindowKind::from_slug(s).unwrap_or(WindowKind::Calendar), want, "slug {s:?}");
    }
    assert_eq!(WindowKind::from_slug("junk").unwrap_or(WindowKind::Calendar), WindowKind::Calendar);
}

/// The title and the glyph each kind wears, pinned to what `WindowKind::label` and `WindowKind::icon` answered
/// before they moved to `ui-theme.toml`'s `window_kind` map (the Trade window's glyph is the ladder, the
/// Data window's title is "Data Manager"). Changing a row of the table changes a window's title bar, and
/// this test is what says so by name; it also holds that every kind is listed, so a new kind is not left
/// out of the pin.
#[test]
fn a_window_kind_wears_the_title_and_the_glyph_the_window_kind_map_gives_it() {
    let pinned = [
        (WindowKind::Chart, "Chart", icons::CHART),
        (WindowKind::Trade, "Trade", icons::DOM),
        (WindowKind::Account, "Account", icons::ACCOUNT),
        (WindowKind::Options, "Options", icons::OPTIONS),
        (WindowKind::News, "News", icons::NEWS),
        (WindowKind::Calendar, "Calendar", icons::CALENDAR),
        (WindowKind::Data, "Data Manager", icons::DATA),
        (WindowKind::Studio, "Studio", icons::STUDIO),
        (WindowKind::Polymarket, "Polymarket", icons::POLYMARKET),
        (WindowKind::Settings, "Settings", icons::SETTINGS),
    ];
    assert_eq!(pinned.len(), WindowKind::ALL.len(), "a kind is missing from the pin");
    for (kind, title, glyph) in pinned {
        assert_eq!(kind.label(), title, "{kind:?}: the title");
        assert_eq!(kind.icon(), glyph, "{kind:?}: the glyph");
    }
}

/// The map is the windows' own: every kind reads the row of its own name, no two share one, and no row of
/// the `window_kind` map is left without a kind.
#[test]
fn every_kind_has_its_own_window_kind_row_and_every_row_its_kind() {
    for k in WindowKind::ALL {
        assert_eq!(k.row().key, format!("{k:?}").to_uppercase(), "{k:?} reads another row");
        assert_eq!(
            WindowKind::ALL.iter().filter(|o| o.row() == k.row()).count(),
            1,
            "{k:?}'s row is shared"
        );
    }
    for row in maps::window_kind::ALL {
        assert!(WindowKind::ALL.iter().any(|k| k.row() == *row), "{} is no window kind", row.key);
    }
}

/// A kind's title and glyph are both written in its row: `WindowKind::label` and `WindowKind::icon` read them and
/// have no answer for a row that lacks one.
#[test]
fn every_window_kind_row_carries_a_title_and_a_glyph() {
    for row in maps::window_kind::ALL {
        assert!(row.word.is_some_and(|w| !w.is_empty()), "{}: no title", row.key);
        assert!(row.icon().is_some(), "{}: no glyph of the registry", row.key);
    }
}
