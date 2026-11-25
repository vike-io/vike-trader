use super::*;

const DAY: i64 = MS_PER_DAY;
/// 2024-01-02.
const JAN2: i64 = 19_724 * DAY;

fn spec() -> FetchSpec {
    FetchSpec {
        format: "dukascopy-bi5".to_string(),
        dataset: "EURUSD".to_string(),
        from_day: JAN2,
        to_day: JAN2,
        go: false,
        concurrency: FETCH_DEFAULT_CONCURRENCY,
    }
}

#[test]
fn a_plain_spec_passes_and_each_rule_refuses_by_name() {
    assert_eq!(validate_fetch_spec(&spec()), Ok(()));
    let bad = |edit: fn(&mut FetchSpec)| {
        let mut s = spec();
        edit(&mut s);
        validate_fetch_spec(&s).expect_err("refused")
    };
    assert!(bad(|s| s.dataset = "../x".to_string()).contains("dataset"));
    assert!(bad(|s| s.from_day += 1).contains("START of a UTC day"));
    assert!(bad(|s| s.from_day += DAY).contains("AFTER"));
    assert!(bad(|s| s.concurrency = 0).contains("concurrency"));
    assert!(bad(|s| s.concurrency = FETCH_MAX_CONCURRENCY + 1).contains("concurrency"));
}

/// A plan may span years (it downloads nothing); a download is one month at most.
#[test]
fn only_a_downloading_request_is_held_to_the_month() {
    let mut year = spec();
    year.to_day = JAN2 + 365 * DAY;
    assert_eq!(validate_fetch_spec(&year), Ok(()));
    year.go = true;
    assert!(validate_fetch_spec(&year).expect_err("refused").contains("FETCH_MAX_DAYS"));
    let mut month = spec();
    month.go = true;
    month.to_day = JAN2 + (FETCH_MAX_DAYS - 1) * DAY;
    assert_eq!(validate_fetch_spec(&month), Ok(()));
}

/// `go` has no default: a frame without it does not decode, so it can read as neither a plan nor a
/// download.
#[test]
fn a_frame_that_omits_go_does_not_decode() {
    let with = serde_json::to_value(spec()).expect("encode");
    let mut without = with.clone();
    without.as_object_mut().expect("an object").remove("go");
    assert!(serde_json::from_value::<FetchSpec>(with).is_ok());
    assert!(serde_json::from_value::<FetchSpec>(without).is_err());
}

#[test]
fn the_plan_sums_the_missing_bytes() {
    let plan = FetchPlan {
        source: "https://bucket.s3.eu-west-1.amazonaws.com".to_string(),
        server_dir: "/p/market_data/imports/dukascopy-bi5/EURUSD".to_string(),
        list_requests: 1,
        listed: 3,
        listed_bytes: 600,
        held: 1,
        missing: vec![
            FetchFile { day: JAN2, key: "EURUSD/2024/00/02_ticks.bi5".to_string(), bytes: 250 },
            FetchFile {
                day: JAN2 + DAY,
                key: "EURUSD/2024/00/03_ticks.bi5".to_string(),
                bytes: 150,
            },
        ],
    };
    assert_eq!(plan.missing_bytes(), 400);
}
