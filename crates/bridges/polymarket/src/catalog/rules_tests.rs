use super::*;
use crate::catalog::gamma::{parse_gamma_markets, parse_keyset_page};

/// A resolved market in Gamma's documented `/markets` shape (fields this module reads, beside the
/// catalog's own).
fn resolved_json() -> serde_json::Value {
    serde_json::json!([{
        "id": "253591",
        "question": "Will the incumbent win?",
        "conditionId": "0xdd22472e552920b8438158ea7238bfadfa4f736aa4cee91a6b86c39ead110917",
        "slug": "will-the-incumbent-win",
        "description": "This market will resolve to \"Yes\" if the incumbent wins. Otherwise \"No\".",
        "resolutionSource": "https://example.test/results",
        "startDate": "2024-01-04T22:58:00Z",
        "createdAt": "2024-01-04T22:57:14.394Z",
        "endDate": "2024-11-05T12:00:00Z",
        "closedTime": "2024-11-06 15:43:47+00",
        "umaEndDate": "2024-11-07T09:00:00Z",
        "active": true,
        "closed": true,
        "outcomes": "[\"Yes\", \"No\"]",
        "outcomePrices": "[\"1\", \"0\"]",
        "clobTokenIds": "[\"111222333444555666777\", \"888999000111222333444\"]",
        "events": [{ "id": "1", "slug": "presidential-election", "title": "Election" }]
    }])
}

fn resolved() -> GammaMarket {
    parse_gamma_markets(&resolved_json()).remove(0)
}

#[test]
fn the_parser_keeps_the_rules_the_resolution_source_and_the_clocks() {
    let m = resolved();
    assert!(m.description.starts_with("This market will resolve"));
    assert_eq!(m.resolution_source, "https://example.test/results");
    assert_eq!(m.start_date, "2024-01-04T22:58:00Z");
    assert_eq!(m.created_at, "2024-01-04T22:57:14.394Z");
    assert_eq!(m.closed_time, "2024-11-06 15:43:47+00");
    assert_eq!(m.uma_end_date, "2024-11-07T09:00:00Z");
    let bare = parse_gamma_markets(&serde_json::json!([{ "question": "q", "conditionId": "0x1" }]));
    assert!(
        bare[0].description.is_empty()
            && bare[0].closed_time.is_empty()
            && bare[0].uma_end_date.is_empty(),
        "absent is empty"
    );
}

#[test]
fn a_gamma_instant_reads_in_every_utc_spelling_and_never_guesses_another() {
    let close = gamma_instant_ms("2024-11-06 15:43:47+00").unwrap();
    assert_eq!(close, vike_model::time::parse_utc_timestamp_ms("2024-11-06T15:43:47Z").unwrap());
    assert_eq!(gamma_instant_ms("2024-11-06T15:43:47Z"), Some(close));
    assert_eq!(gamma_instant_ms("2024-11-06T15:43:47+00:00"), Some(close));
    assert_eq!(gamma_instant_ms("2024-11-06 15:43:47.394512+00"), Some(close + 394));
    assert_eq!(gamma_instant_ms("2024-01-04T22:57:14.394Z").map(|ms| ms % 1000), Some(394));
    for refused in [
        "",
        "2024-11-05",
        "2024-11-06 15:43:47+02",
        "2024-11-06T15:43:47",
        "2024-02-30T00:00:00Z",
        "x+00",
    ] {
        assert_eq!(gamma_instant_ms(refused), None, "{refused:?}");
    }
}

#[test]
fn a_resolved_market_names_its_winner_and_a_split_one_every_payout() {
    let m = resolved();
    assert_eq!(final_outcome(&m).as_deref(), Some("Yes"));
    let split = GammaMarket { outcome_prices: vec![0.5, 0.5], ..m.clone() };
    assert_eq!(final_outcome(&split).as_deref(), Some("Yes=0.5, No=0.5"));
    let last_trade = GammaMarket { outcome_prices: vec![0.9995, 0.0005], ..m.clone() };
    assert_eq!(final_outcome(&last_trade), None, "closed at its last trade: not resolved yet");
    let open = GammaMarket { closed: false, ..m.clone() };
    assert_eq!(final_outcome(&open), None, "an open market has no outcome");
    let ragged = GammaMarket { outcome_prices: vec![1.0], ..m };
    assert_eq!(
        final_outcome(&ragged),
        None,
        "prices that do not align with the outcomes say nothing"
    );
}

#[test]
fn a_rules_row_carries_every_column_of_its_kind() {
    let m = resolved();
    let row = rules_row(&m, 42);
    assert_eq!(row.received_ms, 42);
    assert_eq!(row.kind, TEXT_KIND_POLYMARKET_RULES);
    assert_eq!(row.source, RULES_SOURCE);
    assert_eq!(row.source_id, m.condition_id);
    assert_eq!(row.subject, "111222333444555666777 888999000111222333444");
    assert_eq!(row.title, "Will the incumbent win?");
    assert_eq!(row.summary, "https://example.test/results");
    assert_eq!(row.body, m.description);
    assert_eq!(row.period, "2024-11-05T12:00:00Z");
    assert_eq!(row.tag_list().collect::<Vec<_>>(), ["Yes", "No"]);
    assert_eq!(
        row.url,
        "https://polymarket.com/event/presidential-election/will-the-incumbent-win"
    );
    assert_eq!(row.published_ms, gamma_instant_ms("2024-01-04T22:58:00Z").unwrap());
    assert!(row.actual.is_empty() && row.amends.is_empty());
}

/// A closed market's rules are received at its close, never earlier, and name no outcome; the start stays
/// the publication time a study can read, and a close that cannot be read gives no row at all.
#[test]
fn an_archived_markets_rules_are_received_at_its_close() {
    let m = resolved();
    let rows = archived_rows(&m, 0);
    let row = &rows[0];
    assert_eq!(row.received_ms, gamma_instant_ms(&m.closed_time).unwrap());
    assert_eq!(row.published_ms, gamma_instant_ms(&m.start_date).unwrap());
    assert!(row.actual.is_empty() && row.amends.is_empty(), "the close names no outcome");
    assert_eq!(
        row.text_id(),
        rules_row(&m, 0).text_id(),
        "the live rows and the archive are one text"
    );

    let created_only = GammaMarket { start_date: String::new(), ..m.clone() };
    assert_eq!(
        archived_rows(&created_only, 0)[0].published_ms,
        gamma_instant_ms(&m.created_at).unwrap(),
        "no start: the creation is the publication"
    );
    let unresolved = GammaMarket { outcome_prices: vec![0.9, 0.1], ..m.clone() };
    let rows = archived_rows(&unresolved, 0);
    assert_eq!(rows.len(), 1, "an unresolved market keeps its rules and has no outcome row");
    assert!(rows[0].actual.is_empty());
    let no_close = GammaMarket { closed_time: "soon".into(), ..m };
    assert!(archived_rows(&no_close, 0).is_empty(), "a close that cannot be read is never guessed");
}

/// **Lookahead 6** (`REVIEW-LOOKAHEAD`): a market that CLOSED at C and RESOLVED at R > C is its rules at C
/// with no outcome, then its outcome as a later row of the same text at R (`umaEndDate`), so a decision
/// at R - 1 reads the rules and no outcome. A market whose resolution Gamma does not state gets its
/// outcome at the archive's FETCH time, and a resolution stated before the close is taken at the close.
#[test]
fn an_archived_outcome_is_received_at_its_resolution_never_at_its_close() {
    use vike_data::text_log::POLYMARKET_RULES_ARCHIVE_COLLECTOR;
    use vike_data::{HistoryStore, HistoryStoreMemory};
    let series = (TEXT_KIND_POLYMARKET_RULES, POLYMARKET_RULES_ARCHIVE_COLLECTOR);
    let m = resolved();
    let closed = gamma_instant_ms(&m.closed_time).unwrap();
    let resolution = gamma_instant_ms(&m.uma_end_date).unwrap();
    assert!(resolution > closed, "the fixture resolves after it closes");
    let fetched = resolution + 30 * DAY;

    let rows = archived_rows(&m, fetched);
    assert_eq!(rows.iter().map(|r| r.received_ms).collect::<Vec<_>>(), [closed, resolution]);
    assert!(rows[0].actual.is_empty() && rows[0].amends.is_empty(), "the close names no outcome");
    assert_eq!(rows[1].actual, "Yes");
    assert_eq!(rows[1].amends, m.condition_id, "the outcome row amends the rules row");
    assert_eq!(rows[0].text_id(), rows[1].text_id(), "one text, the outcome its later row");
    assert_eq!(rows[1].body, m.description, "the outcome row keeps the rules");

    // The point-in-time read a study takes over one market's rows: the last row received at or before the
    // decision is the version known then.
    let stored = |rows: &[TextRow]| {
        let store = HistoryStoreMemory::new();
        store.append_texts(series.0, series.1, rows, Some("market-rules:polymarket:day")).unwrap();
        store
    };
    let known_at = |store: &HistoryStoreMemory, decision: i64| {
        store
            .texts_as_of(series.0, series.1, decision, 365 * DAY)
            .unwrap()
            .last()
            .map(|r| r.actual.clone())
    };
    let store = stored(&rows);
    assert_eq!(known_at(&store, closed - 1), None, "nothing before the close");
    assert_eq!(
        known_at(&store, closed).as_deref(),
        Some(""),
        "the rules from the close, no outcome"
    );
    assert_eq!(
        known_at(&store, resolution - 1).as_deref(),
        Some(""),
        "a decision before the resolution reads no outcome"
    );
    assert_eq!(known_at(&store, resolution).as_deref(), Some("Yes"), "known from the resolution");

    let unstated = GammaMarket { uma_end_date: String::new(), ..m.clone() };
    let received: Vec<i64> =
        archived_rows(&unstated, fetched).iter().map(|r| r.received_ms).collect();
    assert_eq!(
        received,
        [closed, fetched],
        "no stated resolution: the fetch time, never the close"
    );
    let unreadable = GammaMarket { uma_end_date: "soon".into(), ..m.clone() };
    assert_eq!(
        archived_rows(&unreadable, fetched)[1].received_ms,
        fetched,
        "unreadable: the fetch"
    );
    let before_close = GammaMarket { uma_end_date: "2024-11-01T00:00:00Z".into(), ..m.clone() };
    assert_eq!(
        archived_rows(&before_close, fetched)[1].received_ms,
        closed,
        "never before the close, so the rules row never follows the outcome"
    );

    // MEASURED (2026-11-17): `umaEndDate` equals `closedTime`. Both rows share the instant, and the
    // append's order makes the outcome the version known at it.
    let measured = GammaMarket {
        closed_time: "2025-03-14 13:53:31+00".into(),
        uma_end_date: "2025-03-14T13:53:31Z".into(),
        ..m
    };
    let rows = archived_rows(&measured, fetched);
    let at = vike_model::time::parse_utc_timestamp_ms("2025-03-14T13:53:31Z").unwrap();
    assert_eq!(rows.iter().map(|r| r.received_ms).collect::<Vec<_>>(), [at, at]);
    let store = stored(&rows);
    assert_eq!(known_at(&store, at - 1), None);
    assert_eq!(known_at(&store, at).as_deref(), Some("Yes"));
}

/// **The spellings MEASURED on a live `/markets?closed=true&end_date_min=…` page (coordinator, the CI box,
/// 2026-11-17)**: `closedTime` is `2025-01-01 10:23:14+00` (a space, an offset of hours only) and `endDate`
/// is RFC 3339; a market resolved EARLY closed months before its scheduled end. Its row is received at the
/// close, never at the end it is fetched by.
#[test]
fn the_measured_gamma_spellings_read_and_an_early_close_is_the_row_time() {
    assert_eq!(
        gamma_instant_ms("2025-01-01 10:23:14+00"),
        vike_model::time::parse_utc_timestamp_ms("2025-01-01T10:23:14Z")
    );
    let early = GammaMarket {
        closed: true,
        closed_time: "2024-10-21 16:05:31+00".into(),
        end_date: "2025-01-01T12:00:00Z".into(),
        ..resolved()
    };
    let rows = archived_rows(&early, 0);
    let row = rows.first().expect("the measured close reads");
    let closed = vike_model::time::parse_utc_timestamp_ms("2024-10-21T16:05:31Z").unwrap();
    assert_eq!(row.received_ms, closed, "the row is known from the close");
    assert!(row.received_ms < gamma_instant_ms(&early.end_date).unwrap(), "months before the end");
    let day = gamma_instant_ms("2025-01-01T00:00:00Z").unwrap();
    let kept =
        closed_markets_ending(day, day + DAY - 1, |_| Ok((vec![early.clone()], None))).unwrap();
    assert_eq!(kept.len(), 1, "an early-closed market is in the day of its scheduled END");
}

fn market(condition_id: &str, end: &str, closed: bool) -> GammaMarket {
    GammaMarket {
        question: condition_id.to_string(),
        condition_id: condition_id.to_string(),
        end_date: end.to_string(),
        closed,
        ..Default::default()
    }
}

const DAY: i64 = 86_400_000;

#[test]
fn the_archive_walk_reads_every_page_and_keeps_only_the_windows_closed_markets() {
    let start = gamma_instant_ms("2024-11-05T00:00:00Z").unwrap();
    let end = start + DAY - 1;
    let asked = std::cell::RefCell::new(Vec::new());
    let markets = closed_markets_ending(start, end, |cursor| {
        asked.borrow_mut().push(cursor.to_string());
        match cursor {
            "" => Ok((vec![market("a", "2024-11-05T12:00:00Z", true)], Some("p2".into()))),
            "p2" => Ok((
                vec![
                    market("a", "2024-11-05T12:00:00Z", true),
                    market("b", "2024-11-06T00:00:00Z", true),
                    market("c", "2024-11-05T23:00:00Z", false),
                    market("d", "", true),
                ],
                Some("p3".into()),
            )),
            _ => Ok((vec![market("e", "2024-11-05T00:00:00Z", true)], None)),
        }
    })
    .unwrap();
    assert_eq!(*asked.borrow(), ["", "p2", "p3"], "each page after the one Gamma named");
    let kept: Vec<&str> = markets.iter().map(|m| m.condition_id.as_str()).collect();
    assert_eq!(kept, ["a", "d", "e"], "b ends after the window, c is open, a is kept once");
}

/// MEASURED (2026-11-17): Gamma lists at most 100 markets a page for `limit=500`, which once ended the
/// walk at its first page (100 markets a day stored), and a day holds tens of thousands (27,172 ending
/// 2026-10-05). A short page is not the end; a page naming no next cursor is.
#[test]
fn a_page_gamma_caps_at_100_is_not_the_end_and_every_market_is_read_once() {
    let ended: Vec<GammaMarket> =
        (0..250).map(|i| market(&format!("m{i}"), "2024-11-05T12:00:00Z", true)).collect();
    let start = gamma_instant_ms("2024-11-05T00:00:00Z").unwrap();
    let asked = std::cell::RefCell::new(Vec::new());
    let markets = closed_markets_ending(start, start + DAY - 1, |cursor| {
        asked.borrow_mut().push(cursor.to_string());
        let at: usize = if cursor.is_empty() { 0 } else { cursor.parse().unwrap() };
        let page: Vec<GammaMarket> = ended.iter().skip(at).take(100).cloned().collect();
        let next = (at + page.len() < ended.len()).then(|| (at + page.len()).to_string());
        Ok((page, next))
    })
    .unwrap();
    assert_eq!(*asked.borrow(), ["", "100", "200"], "the page naming no cursor ends it");
    let ids =
        |list: &[GammaMarket]| list.iter().map(|m| m.condition_id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&markets), ids(&ended), "all 250, each once, in Gamma's order");
}

#[test]
fn the_archive_walk_never_keeps_a_short_window() {
    let failing_later = closed_markets_ending(0, DAY, |cursor| {
        if cursor.is_empty() {
            Ok((vec![market("a", "", true)], Some("p2".into())))
        } else {
            Err("timeout".into())
        }
    });
    assert_eq!(
        failing_later.unwrap_err(),
        PolymarketRulesRefusal::PageUnread { why: "timeout".to_string() },
        "a later page failing fails the window"
    );
    let pages = std::cell::Cell::new(0usize);
    let endless = closed_markets_ending(0, DAY, |cursor| {
        pages.set(pages.get() + 1);
        Ok((Vec::new(), Some(format!("{cursor}x"))))
    });
    assert_eq!(
        endless.unwrap_err(),
        PolymarketRulesRefusal::ListingPastCeiling { pages: RULES_MAX_PAGES },
        "a listing past the ceiling is refused"
    );
    assert_eq!(pages.get(), RULES_MAX_PAGES, "and no page past the ceiling is read");
    let echoed = closed_markets_ending(0, DAY, |_| Ok((Vec::new(), Some("same".into()))));
    assert_eq!(
        echoed.unwrap_err().id(),
        crate::refusals::PolymarketRulesRefusalId::CursorRepeated,
        "a cursor named twice is refused"
    );
}

#[test]
fn the_archive_query_asks_closed_markets_ending_in_the_window() {
    let start = gamma_instant_ms("2024-11-05T00:00:00Z").unwrap();
    let q = gamma_closed_ending_query(start, start + DAY - 1, 500);
    assert_eq!(
        q,
        "closed=true&end_date_min=2024-11-05T00%3A00%3A00Z&end_date_max=2024-11-05T23%3A59%3A59Z\
         &limit=500"
    );
}

/// The keyset answer's measured shape: the markets under `markets`, the next page's cursor under
/// `next_cursor`, null on the last page; an answer with no market list is an error.
#[test]
fn a_keyset_page_reads_its_markets_and_its_cursor_and_a_refusal_is_no_page() {
    let page = serde_json::json!({ "markets": resolved_json(), "next_cursor": "qkMh-x_9" });
    let (markets, next) = parse_keyset_page(&page).unwrap();
    assert_eq!(markets.len(), 1);
    assert_eq!(next.as_deref(), Some("qkMh-x_9"));
    for last in [
        serde_json::json!({ "markets": [], "next_cursor": null }),
        serde_json::json!({ "markets": [] }),
        serde_json::json!({ "markets": [], "next_cursor": "" }),
    ] {
        assert_eq!(parse_keyset_page(&last).unwrap().1, None, "{last}");
    }
    let refusal = serde_json::json!({ "type": "validation error", "error": "offset too large" });
    assert!(parse_keyset_page(&refusal).is_err());
    assert!(parse_keyset_page(&resolved_json()).is_err(), "the offset route's bare array");
}

/// One UTC day of a 5-minute series is its 288 windows, the first at midnight and the last at 23:55, slugged
/// by their start in epoch seconds (the measured `btc-updown-5m-1792026000`); a 15-minute series is 96.
#[test]
fn a_series_day_is_every_window_start_of_the_day_slugged_by_its_seconds() {
    // 2026-10-15 00:00:00 UTC; the measured market `btc-updown-5m-1792026000` is its 01:00 window.
    let day = gamma_instant_ms("2026-10-15T00:00:00Z").unwrap();
    let slugs = series_slugs("btc-updown-5m", day, day + DAY - 1).unwrap();
    assert_eq!(slugs.len(), 288);
    assert_eq!(slugs[0], "btc-updown-5m-1792022400");
    assert_eq!(slugs[12], "btc-updown-5m-1792026000", "the measured market");
    assert_eq!(slugs[287], "btc-updown-5m-1792108500", "23:55, the day's last window");
    assert_eq!(series_slugs("eth-updown-15m", day, day + DAY - 1).unwrap().len(), 96);
    let inside = series_slugs("sol-updown-5m", day + 1, day + 600_000).unwrap();
    assert_eq!(
        inside,
        [
            format!("sol-updown-5m-{}", day / 1000 + 300),
            format!("sol-updown-5m-{}", day / 1000 + 600)
        ],
        "a window start off the grid rounds up to the next window"
    );
}

/// A name that states no window length in minutes is refused in words, and nothing is asked.
#[test]
fn a_series_name_without_a_window_length_is_refused() {
    for refused in ["gamma", "archive", "btc-updown-1h", "btc-updown-0m", ""] {
        let why = series_slugs(refused, 0, DAY).unwrap_err();
        assert!(why.contains("does not end in an interval"), "{refused:?}: {why}");
    }
    let urls = vike_venues::venue_urls::VenueUrls::seed("polymarket");
    let source = MarketRulesSourcePolymarket::from_urls(&urls).unwrap();
    assert!(matches!(source.closed_of_series("gamma", 0, DAY), Err(SourceError::Refused(_))));
    assert!(matches!(source.closed_of_series("btc-updown-5m", 2, 1), Err(SourceError::Refused(_))));
}

fn slugged(slug: &str, closed: bool) -> GammaMarket {
    GammaMarket { slug: slug.to_string(), ..market(&format!("0x{slug}"), "", closed) }
}

/// A day's 288 slugs are asked 100 at a time (three requests); a page that answers every slug it asked
/// ends its chunk, though Gamma names a cursor after it (MEASURED: the page it names is empty); a slug Gamma
/// lists nothing for is absent, and a market not asked or not closed is dropped.
#[test]
fn the_slugs_are_asked_100_a_request_and_a_full_page_is_the_whole_chunk() {
    let slugs = series_slugs("btc-updown-5m", 0, DAY - 1).unwrap();
    let asked = std::cell::RefCell::new(Vec::new());
    let markets = closed_markets_of_slugs(&slugs, |chunk, cursor| {
        asked.borrow_mut().push((chunk.len(), chunk[0].clone(), cursor.to_string()));
        // Every slug but the very first is listed closed; a stranger and the first, still open, ride along.
        let mut rows: Vec<GammaMarket> =
            chunk.iter().filter(|s| **s != slugs[0]).map(|s| slugged(s, true)).collect();
        if chunk.len() < 100 {
            rows.push(slugged("eth-updown-5m-0", true));
            rows.push(slugged(&slugs[0], false));
        }
        let next = (rows.len() >= chunk.len()).then(|| "after-a-full-page".to_string());
        Ok((rows, next))
    })
    .unwrap();
    assert_eq!(
        *asked.borrow(),
        [
            (100, slugs[0].clone(), String::new()),
            (100, slugs[100].clone(), String::new()),
            (88, slugs[200].clone(), String::new()),
        ],
        "100, 100, 88, and no cursor followed after a page answering every slug"
    );
    assert_eq!(
        markets.len(),
        287,
        "the unlisted window is absent, the stranger and the open dropped"
    );
    assert!(markets.iter().all(|m| m.slug.starts_with("btc-updown-5m-")));
}

/// A page SHORTER than its slugs is followed while Gamma names a cursor (a lowered page size must not lose
/// the rest), and a failing page fails the whole answer.
#[test]
fn a_short_page_with_a_cursor_is_followed_and_a_failure_keeps_nothing() {
    let slugs = series_slugs("btc-updown-5m", 0, 3_600_000 - 1).unwrap();
    assert_eq!(slugs.len(), 12);
    let markets = closed_markets_of_slugs(&slugs, |chunk, cursor| match cursor {
        "" => Ok((chunk[..5].iter().map(|s| slugged(s, true)).collect(), Some("p2".into()))),
        _ => Ok((chunk[5..].iter().map(|s| slugged(s, true)).collect(), None)),
    })
    .unwrap();
    assert_eq!(markets.len(), 12);
    let failed = closed_markets_of_slugs(&slugs, |_, cursor| {
        if cursor.is_empty() { Ok((Vec::new(), Some("p2".into()))) } else { Err("timeout".into()) }
    });
    assert_eq!(
        failed.unwrap_err(),
        PolymarketRulesRefusal::PageUnread { why: "timeout".to_string() }
    );
}

#[test]
fn the_slug_query_asks_the_closed_markets_of_every_slug_on_one_page() {
    let q = gamma_closed_slugs_query(&["btc-updown-5m-1792026000".to_string(), "a b".to_string()]);
    assert_eq!(q, "closed=true&limit=100&slug=btc-updown-5m-1792026000&slug=a%20b");
}

#[test]
fn the_source_refuses_an_inverted_window_and_a_name_that_is_no_token() {
    let urls = vike_venues::venue_urls::VenueUrls::seed("polymarket");
    let source =
        MarketRulesSourcePolymarket::from_urls(&urls).expect("the seed carries the gamma row");
    assert!(matches!(source.closed_ending(2, 1), Err(SourceError::Refused(_))));
    assert!(matches!(source.of_token("will-it-happen"), Err(SourceError::Refused(_))));
    assert_eq!(source.venue(), "polymarket");
    assert!(
        MarketRulesSourcePolymarket::from_urls(&vike_venues::venue_urls::VenueUrls::default())
            .is_err(),
        "no gamma row: no source"
    );
}
