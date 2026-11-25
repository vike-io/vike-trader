use super::*;
use crate::refusals::DeribitHistoryRefusalId;

/// A recorded page (BTC DVOL, `1D`, 2021-03-20..2021-04-05): the index starts on 2021-03-24, each row is
/// one bar with no volume, ascending.
#[test]
fn a_recorded_dvol_page_maps_to_bars() {
    let bars =
        parse_deribit_dvol(include_str!("../tests/fixtures/deribit_dvol_btc_1d.json")).unwrap();
    assert_eq!(bars.len(), 13);
    let first = &bars[0];
    assert_eq!(first.timestamp, 1_616_544_000_000);
    assert_eq!((first.open, first.high, first.low, first.close), (84.8, 95.94, 80.52, 95.04));
    assert_eq!(first.volume, 0.0);
    assert!(bars.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
}

/// The venue's refusal and a malformed row are errors, never an empty or short answer.
#[test]
fn a_dvol_error_or_a_short_row_fails() {
    let refused = r#"{"jsonrpc":"2.0","error":{"code":-32602,"data":{"reason":"unsupported resolution","param":"resolution"},"message":"Invalid params"}}"#;
    assert_eq!(
        parse_deribit_dvol(refused).unwrap_err(),
        DeribitHistoryRefusal::DvolVenueError {
            msg: "Invalid params".to_string(),
            reason: "unsupported resolution".to_string(),
        }
    );
    let short = r#"{"result":{"data":[[1616544000000,84.8,95.94,80.52]],"continuation":null}}"#;
    assert_eq!(parse_deribit_dvol(short).unwrap_err().id(), DeribitHistoryRefusalId::DvolRow);
    assert!(
        parse_deribit_dvol(r#"{"result":{"data":[],"continuation":null}}"#).unwrap().is_empty()
    );
}

/// `DVOL-BTC` is the feed's spelling; nothing else is a DVOL symbol. The five intervals the venue serves
/// map to its words, and any other is refused before anything is sent, through the kline source too.
#[test]
fn the_dvol_symbol_and_its_intervals() {
    assert_eq!(dvol_currency("DVOL-BTC"), Some("BTC"));
    assert_eq!(dvol_currency("DVOL-ETH"), Some("ETH"));
    for other in ["BTC-PERPETUAL", "DVOL-", "DVOL-btc", "BTC-DVOL"] {
        assert_eq!(dvol_currency(other), None, "{other}");
    }
    assert_eq!(
        crate::feed::dvol::dvol_symbol("btc_usd").as_deref().and_then(dvol_currency),
        Some("BTC"),
        "the feed's spelling reads back"
    );
    let words: Vec<&str> =
        ["1s", "1m", "1h", "12h", "1d"].iter().map(|i| dvol_resolution(i).unwrap()).collect();
    assert_eq!(words, ["1", "60", "3600", "43200", "1D"]);
    use vike_data::source::{KlineSource, SourceError};
    let source = KlineSourceDeribit::from_urls(&VenueUrls::seed(VENUE)).unwrap();
    let err = source.fetch("DVOL-BTC", "5m", 0, 1).unwrap_err();
    let refusal = DeribitHistoryRefusal::DvolInterval { interval: "5m".to_string() };
    assert_eq!(err, SourceError::Refused(refusal.carried()));
}

/// The backward walk over scripted pages (the END-ANCHORED endpoint answers the newest rows of a window):
/// every page asked ends one millisecond below the previous page's oldest row, and the walk stops at an
/// empty page.
#[test]
fn the_dvol_pages_walk_backward() {
    let hour = 3_600_000;
    let mut asked = Vec::new();
    let (bars, _) = kline_page_walk_backward(
        VENUE,
        "DVOL-BTC",
        "1h",
        0,
        10 * hour,
        Pacer::fallback(Duration::ZERO),
        |end| {
            asked.push(end);
            let newest = end - end.rem_euclid(hour);
            let oldest = (newest - 2 * hour).max(5 * hour);
            Ok((oldest..=newest)
                .step_by(hour as usize)
                .filter(|&t| t <= end && newest >= 5 * hour)
                .map(|t| kline_to_bar(t, 1.0, 1.0, 1.0, 1.0, 0.0))
                .collect())
        },
    )
    .unwrap();
    let times: Vec<i64> = bars.iter().map(|b| b.timestamp / hour).collect();
    assert_eq!(times, [5, 6, 7, 8, 9, 10]);
    assert_eq!(asked, [10 * hour, 8 * hour - 1, 5 * hour - 1]);
}

/// Live, a day of BTC DVOL hours (`cargo test -p vike-deribit --lib history_dvol -- --ignored`).
#[test]
#[ignore = "network: reads www.deribit.com"]
fn live_a_day_of_btc_dvol_hours() {
    use vike_data::source::KlineSource;
    let source = KlineSourceDeribit::from_urls(&VenueUrls::seed(VENUE)).unwrap();
    // 2024-06-03, and a window wider than one page (60 days of hours) to cross a page boundary
    let day = source.fetch("DVOL-BTC", "1h", 1_717_372_800_000, 1_717_459_199_999).unwrap();
    assert_eq!(day.len(), 24);
    let wide = source.fetch("DVOL-BTC", "1h", 1_704_067_200_000, 1_709_251_200_000).unwrap();
    assert_eq!(
        wide.len(),
        1_441,
        "every hour of 2024-01-01..2024-03-01 inclusive, across two pages"
    );
}
