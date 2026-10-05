use super::*;

fn pt(ts_ms: i64) -> FundingRatePoint {
    FundingRatePoint { ts_ms, rate: 0.0001, premium: None }
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
    assert_eq!(by_time[0].ts_ms, 1_700_000_000_000);
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
    let got = page_funding_forward(0, 100, 2, |cursor| {
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
    assert_eq!(got.iter().map(|p| p.ts_ms).collect::<Vec<_>>(), vec![10, 20, 30, 40, 50]);
}

#[test]
fn the_pager_keeps_only_the_window_and_passes_a_refusal_through() {
    let got =
        page_funding_forward(15, 35, 10, |_| Ok(vec![pt(10), pt(20), pt(30), pt(40)])).unwrap();
    assert_eq!(got.iter().map(|p| p.ts_ms).collect::<Vec<_>>(), vec![20, 30]);
    let err =
        page_funding_forward(0, 10, 10, |_| Err(SourceError::Refused("no".into()))).unwrap_err();
    assert_eq!(err, SourceError::Refused("no".into()));
}
