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

/// The legacy `VIKE_TOOL=<slug>` grid: every value the replaced match named still resolves to
/// the same kind, with `.unwrap_or(Calendar)` supplying the old `_ =>` fallback for junk.
#[test]
fn legacy_vike_tool_vocabulary_is_unchanged() {
    for (s, want) in [
        ("trade", WinKind::Trade),
        ("dom", WinKind::Dom),
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
