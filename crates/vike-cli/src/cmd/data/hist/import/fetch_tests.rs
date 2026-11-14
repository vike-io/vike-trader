use vike_datahub_client::archive_fetch::{FetchFailure, FetchFile};
use vike_model::MS_PER_DAY;

use super::*;

const DAY: i64 = MS_PER_DAY;
/// 2024-01-02.
const JAN2: i64 = 19_724 * DAY;

fn plan(missing: &[(i64, u64)]) -> FetchPlan {
    FetchPlan {
        source: "https://cfg-public-proper-wallaby.s3.eu-west-1.amazonaws.com".to_string(),
        server_dir: "/opt/project/market_data/imports/dukascopy-bi5/EURUSD".to_string(),
        list_requests: 2,
        listed: 3,
        listed_bytes: 900_000,
        held: 3 - missing.len() as u64,
        missing: missing
            .iter()
            .map(|&(day, bytes)| FetchFile { day, key: format!("EURUSD/{day}"), bytes })
            .collect(),
    }
}

/// The estimate is the list prices, summed: 1,000 GETs are $0.0004, 1,000 LISTs $0.005, a GB out $0.09.
#[test]
fn the_cost_is_the_list_prices_summed() {
    let near = |a: f64, b: f64| (a - b).abs() < 1e-12;
    assert!(near(cost_dollars(0, 1000, 0), 0.0004));
    assert!(near(cost_dollars(1000, 0, 0), 0.005));
    assert!(near(cost_dollars(0, 0, 1_073_741_824), 0.09));
    // The vendor page's EUR/USD: 26,586 objects, 2.6 GB.
    let eurusd = cost_dollars(0, 26_586, 2_791_728_742);
    assert!((0.24..0.25).contains(&eurusd), "{eurusd}");
}

#[test]
fn the_plan_names_the_bucket_the_folder_and_what_the_download_costs() {
    let p = plan(&[(JAN2, 300_000), (JAN2 + DAY, 300_000)]);
    let text = plan_lines(&p, "127.0.0.1:7878").join("\n");
    for needle in [
        "datahub at 127.0.0.1:7878",
        "cfg-public-proper-wallaby.s3.eu-west-1.amazonaws.com",
        "Requester Pays: billed to the AWS key stored on the datahub's box",
        "/opt/project/market_data/imports/dukascopy-bi5/EURUSD   (on the DATAHUB's box)",
        "3 daily files in the window",
        "2 LIST requests",
        "held        1 already in the folder",
        "download    2 files (2024-01-02 .. 2024-01-03)",
        "estimated $0.0001",
        "2 GETs at $0.0004 per 1,000",
    ] {
        assert!(text.contains(needle), "missing {needle:?}:\n{text}");
    }
    assert_eq!(confirmation(&p), "fetch 2 files");
}

/// Only the months that hold a missing file are asked for.
#[test]
fn only_a_month_with_a_missing_file_is_fetched() {
    let feb1 = JAN2 + 30 * DAY;
    let months =
        [(JAN2 - DAY, feb1 - DAY), (feb1, feb1 + 28 * DAY), (feb1 + 29 * DAY, feb1 + 59 * DAY)];
    let p = plan(&[(feb1 + 3 * DAY, 1)]);
    assert_eq!(months_to_fetch(&p, &months), vec![months[1]]);
}

#[test]
fn a_failed_file_is_listed_by_day_with_how_to_resume() {
    let done = FetchOutcome {
        fetched: 4,
        bytes: 10,
        get_requests: 9,
        failed: vec![FetchFailure { day: JAN2, why: "HTTP 503 (SlowDown)".to_string() }],
    };
    let text = failure(&done);
    assert!(text.contains("1 file could not be downloaded"), "{text}");
    assert!(text.contains("2024-01-02  HTTP 503 (SlowDown)"), "{text}");
    assert!(text.contains("Run the same command again"), "{text}");
    let line = progress_line(2, 3, JAN2, &done);
    assert!(line.contains("[2/3] 2024-01  downloaded 4 files") && line.contains("1 file FAILED"));
    assert!(summary_line(2, &done, 1).contains("9 GETs and 3 LISTs"));
}
