use super::*;

fn pt(ts_ms: i64) -> FundingRatePoint {
    FundingRatePoint { timestamp_ms: ts_ms, rate: 0.0001, premium: None, mark: None }
}

/// What a funding page walk answers, so the tests name the row and error type once.
type Points = Result<Vec<FundingRatePoint>, SourceError>;

/// The backward walk an end-anchored venue needs: each page ends one ms below the previous page's
/// oldest point, a SHORT page does not stop it (only an empty one, or reaching the start), and the
/// result is the window, ascending, once each.
#[test]
fn the_backward_pager_walks_end_down_until_the_window_start_and_ignores_page_length() {
    let mut ends = Vec::new();
    let got = page_walk_backward(15, 100, never_stuck, |end| -> Points {
        ends.push(end);
        Ok(match end {
            100 => vec![pt(90), pt(100), pt(80)],
            79 => vec![pt(70)],
            69 => vec![pt(60), pt(50), pt(40)],
            39 => vec![pt(30), pt(20), pt(10)],
            other => panic!("unexpected end {other}"),
        })
    })
    .unwrap();
    assert_eq!(ends, vec![100, 79, 69, 39], "a one-row page is not the end of the history");
    assert_eq!(
        got.iter().map(|p| p.timestamp_ms).collect::<Vec<_>>(),
        vec![20, 30, 40, 50, 60, 70, 80, 90, 100]
    );
}

#[test]
fn the_backward_pager_stops_on_an_empty_page_and_an_empty_window() {
    let mut calls = 0;
    let got = page_walk_backward(0, 100, never_stuck, |_| -> Points {
        calls += 1;
        Ok(if calls == 1 { vec![pt(50)] } else { Vec::new() })
    })
    .unwrap();
    assert_eq!((calls, got.len()), (2, 1), "an empty page is the end of the history");
    assert!(
        page_walk_backward(10, 9, never_stuck, |_| -> Points {
            panic!("an empty window asks nothing")
        })
        .unwrap()
        .is_empty()
    );
}

/// What a test walk says when a page does not move its cursor: the shared sentence.
fn stuck_text(stuck: PageWalkStuck) -> SourceError {
    SourceError::Fetch(stuck.text("binance", "BTCUSDT", "1m").into())
}

/// A walk these tests expect to end normally: a stuck page would fail the test loudly.
fn never_stuck<E>(stuck: PageWalkStuck) -> E {
    panic!("no page in this test may repeat: {stuck:?}")
}

/// Owner ruling H-D6: a NON-EMPTY backward page that does not move the cursor (a venue answering rows
/// newer than the asked end, here the same page again) ENDS THE WALK WITH AN ERROR naming the point,
/// never a short success, and asks nothing more.
#[test]
fn a_backward_page_that_does_not_move_the_cursor_is_an_error_naming_the_point() {
    const T: i64 = 1_710_064_800_000; // 2024-03-10T10:00:00Z
    let mut ends = Vec::new();
    let err = page_walk_backward(0, T + 120_000, stuck_text, |end| -> Points {
        ends.push(end);
        Ok(vec![pt(T), pt(T + 60_000)])
    })
    .unwrap_err();
    assert_eq!(ends, vec![T + 120_000, T - 1], "the second page repeated the first");
    assert_eq!(
        err,
        SourceError::Fetch(
            "binance BTCUSDT 1m: the page up to 2024-03-10T09:59:59.999Z repeated; the backfill \
             stopped here"
                .into()
        )
    );
}

/// The forward twin: a FULL page that does not move the cursor is the error, after the rows that did
/// move it.
#[test]
fn a_forward_page_that_does_not_move_the_cursor_is_an_error_naming_the_point() {
    const T: i64 = 1_710_064_800_000; // 2024-03-10T10:00:00Z
    let mut cursors = Vec::new();
    let err = page_walk_forward(T, T + 600_000, 2, stuck_text, |cursor| -> Points {
        cursors.push(cursor);
        Ok(vec![pt(T), pt(T + 60_000)])
    })
    .unwrap_err();
    assert_eq!(cursors, vec![T, T + 60_001], "the second page answered the first page again");
    assert_eq!(
        err,
        SourceError::Fetch(
            "binance BTCUSDT 1m: the page from 2024-03-10T10:01:00.001Z repeated; the backfill \
             stopped here"
                .into()
        )
    );
}

/// An EMPTY page is still the venue's end in both directions, and a SHORT forward page is too, even
/// one that does not move the cursor: neither is a repeat.
#[test]
fn an_empty_page_and_a_short_forward_page_still_end_the_walk_normally() {
    let empty = |_: i64| -> Points { Ok(Vec::new()) };
    assert!(page_walk_forward(0, 100, 2, never_stuck, empty).unwrap().is_empty());
    assert!(page_walk_backward(0, 100, never_stuck, empty).unwrap().is_empty());
    let mut calls = 0;
    let short = page_walk_forward(10, 100, 2, never_stuck, |_| -> Points {
        calls += 1;
        Ok(vec![pt(5)])
    });
    assert!(short.unwrap().is_empty(), "the short page's one row is outside the window");
    assert_eq!(calls, 1, "a short page ends the walk");
}

/// A kline [`Bar`] is walked by its `ts`, the same rule as a funding point: the window, ascending,
/// once each, whatever order the pages came in.
#[test]
fn bars_walk_by_their_open_time() {
    let bar = |ts: i64| Bar {
        timestamp: ts,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    let got = page_walk_backward(10, 40, never_stuck, |end| -> Result<Vec<Bar>, String> {
        Ok(match end {
            40 => vec![bar(40), bar(30), bar(40)],
            29 => vec![bar(10), bar(20), bar(0)],
            other => panic!("unexpected end {other}"),
        })
    })
    .unwrap();
    assert_eq!(got.iter().map(|b| b.timestamp).collect::<Vec<_>>(), vec![10, 20, 30, 40]);
}

/// hyperliquid's rule as a parameter: a row whose next FORWARD page starts one ms past its CLOSE,
/// not past its open time. The window, the order and the de-duplication still read the open time.
#[test]
fn a_row_can_name_its_own_forward_cursor() {
    #[derive(Debug, PartialEq)]
    struct Candle {
        open: i64,
        close: i64,
    }
    impl PageWalkRow for Candle {
        fn ts_ms(&self) -> i64 {
            self.open
        }
        fn next_cursor_ms(&self) -> i64 {
            self.close + 1
        }
    }
    let c = |open: i64| Candle { open, close: open + 9 };
    let mut cursors = Vec::new();
    let got = page_walk_forward(0, 35, 2, never_stuck, |cursor| -> Result<Vec<Candle>, String> {
        cursors.push(cursor);
        Ok(match cursor {
            0 => vec![c(10), c(0)],
            20 => vec![c(20), c(30)],
            other => panic!("unexpected cursor {other} (40 is past the window: never asked)"),
        })
    })
    .unwrap();
    assert_eq!(cursors, vec![0, 20], "each page starts one ms past the greatest close");
    assert_eq!(got, vec![c(0), c(10), c(20), c(30)]);
}

#[test]
fn a_mark_is_a_positive_price_and_an_empty_string_is_none() {
    let body = r#"[{"fundingTime":1,"fundingRate":"0.0001","markPrice":"76038.8"},
                   {"fundingTime":2,"fundingRate":"0.0001","markPrice":""},
                   {"fundingTime":3,"fundingRate":"0.0001"},
                   {"fundingTime":4,"fundingRate":"0.0001","markPrice":"0"}]"#;
    let marks: Vec<_> =
        parse_funding_rate_rows(body, "fundingTime").unwrap().iter().map(|p| p.mark).collect();
    assert_eq!(marks, vec![Some(76_038.8), None, None, None]);
    assert_eq!(positive_price(Some(&serde_json::json!(77_013.63))), Some(77_013.63));
}

#[test]
fn a_source_error_renders_the_way_collect_error_always_did() {
    assert_eq!(SourceError::Fetch("boom".into()).to_string(), "venue fetch: boom");
    assert_eq!(SourceError::Refused("no".into()).to_string(), "refused: no");
}

#[test]
fn both_source_traits_can_sit_in_a_static_registry() {
    fn shareable<T: ?Sized + Send + Sync>() {}
    shareable::<dyn KlineSource>();
    shareable::<dyn FundingRateSource>();
}

#[test]
fn funding_rows_decode_the_timestamp_field_they_are_told_to() {
    let body = r#"[{"time":1700000000000,"fundingRate":"0.0001","premium":"-0.0002"},
                   {"fundingTime":1700028800000,"fundingRate":"0.0003"}]"#;
    let by_time = parse_funding_rate_rows(body, "time").unwrap();
    assert_eq!(by_time.len(), 1, "a row without the named timestamp field is skipped");
    assert_eq!(by_time[0].timestamp_ms, 1_700_000_000_000);
    assert_eq!(by_time[0].premium, Some(-0.0002));
    let by_funding_time = parse_funding_rate_rows(body, "fundingTime").unwrap();
    assert_eq!(by_funding_time.len(), 1);
    assert_eq!(by_funding_time[0].premium, None, "an absent premium is None, never 0.0");
}

#[test]
fn a_non_array_body_is_an_error_and_an_empty_array_is_empty() {
    assert!(parse_funding_rate_rows("{}", "time").is_err());
    assert!(parse_funding_rate_rows("not json", "time").is_err());
    assert_eq!(parse_funding_rate_rows("[]", "time").unwrap(), Vec::new());
}

#[test]
fn the_pager_advances_past_each_page_and_stops_on_a_short_one() {
    let mut cursors = Vec::new();
    let got = page_walk_forward(0, 100, 2, never_stuck, |cursor| -> Points {
        cursors.push(cursor);
        Ok(match cursor {
            0 => vec![pt(10), pt(20)],
            21 => vec![pt(30), pt(40)],
            41 => vec![pt(50)],
            other => panic!("unexpected cursor {other}"),
        })
    })
    .unwrap();
    assert_eq!(
        cursors,
        vec![0, 21, 41],
        "each page starts one ms past the previous one's last point"
    );
    assert_eq!(got.iter().map(|p| p.timestamp_ms).collect::<Vec<_>>(), vec![10, 20, 30, 40, 50]);
}

#[test]
fn the_pager_keeps_only_the_window_and_passes_a_refusal_through() {
    let got = page_walk_forward(15, 35, 10, never_stuck, |_| -> Points {
        Ok(vec![pt(10), pt(20), pt(30), pt(40)])
    })
    .unwrap();
    assert_eq!(got.iter().map(|p| p.timestamp_ms).collect::<Vec<_>>(), vec![20, 30]);
    let err = page_walk_forward(0, 10, 10, never_stuck, |_| -> Points {
        Err(SourceError::Refused("no".into()))
    })
    .unwrap_err();
    assert_eq!(err, SourceError::Refused("no".into()));
}

#[test]
fn a_fill_source_spells_one_stable_word_and_all_lists_every_variant() {
    // No `_` arm: a new variant fails to compile here until it is given a position, and the
    // positions then have to equal `ALL`'s, which catches a variant left out of `ALL`.
    let position = |s: BackfillProvider| match s {
        BackfillProvider::Venue => 0,
        BackfillProvider::Archive => 1,
        BackfillProvider::Databento => 2,
        BackfillProvider::Tardis => 3,
    };
    let positions: Vec<usize> = BackfillProvider::ALL.iter().map(|s| position(*s)).collect();
    assert_eq!(positions, [0, 1, 2, 3]);
    let words: Vec<&str> = BackfillProvider::ALL.iter().map(|s| s.as_str()).collect();
    assert_eq!(words, ["venue", "archive", "databento", "tardis"]);
    let mut sorted = BackfillProvider::ALL;
    sorted.sort();
    assert_eq!(sorted, BackfillProvider::ALL, "declaration order is the derived `Ord`");
}

/// The inner text, whichever variant: what a `CatalogError` or a tick fetch's `String` carries.
#[test]
fn a_source_errors_text_is_its_inner_text_whichever_variant() {
    assert_eq!(SourceError::Fetch("a".into()).text(), "a");
    assert_eq!(SourceError::Refused("b".into()).text(), "b");
}

/// ⚠ The payload's id never reaches the bytes: a carried refusal writes its TEXT alone, the bytes a `String`
/// wrote (`fixtures/gatewayhub_wire/v1_responses.jsonl`'s `FetchFailed` lines), and those bytes read back as free
/// text; the id is the frame's own field. `Display` and `text()` stay the bare words.
#[test]
fn a_source_error_writes_its_text_alone_and_keeps_its_id_in_process() {
    use vike_utility::errors::{CarriedRefusal, Declared};
    let carried = CarriedRefusal::new(Some("ig.history.x".into()), "no such pair".into());
    let refused = SourceError::Refused(carried);
    assert_eq!(refused.declared_id(), Some("ig.history.x"));
    assert_eq!(
        (refused.text(), refused.to_string()),
        ("no such pair", "refused: no such pair".into())
    );
    let bytes = serde_json::to_string(&refused).unwrap();
    assert_eq!(bytes, r#"{"Refused":"no such pair"}"#);
    let back: SourceError = serde_json::from_str(&bytes).unwrap();
    assert_eq!(back, SourceError::Refused("no such pair".into()));
    assert_eq!(back.declared_id(), None);
}
