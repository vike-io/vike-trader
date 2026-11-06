//! IG prices → [`Bar`]. IG quotes each OHLC point as a `{bid, ask}` object, so bars are built
//! from the MID. Timestamps come as `snapshotTimeUTC` ("YYYY-MM-DDTHH:MM:SS") — parsed to epoch-ms
//! with a small civil-date routine (no chrono dependency).

use crate::rest::{IgApiError, IgSession};
use vike_marketdata::Bar;

/// Map a canonical interval to an IG resolution. `None` for one IG lacks.
pub fn resolution(interval: &str) -> Option<&'static str> {
    Some(match interval {
        "1m" => "MINUTE",
        "2m" => "MINUTE_2",
        "3m" => "MINUTE_3",
        "5m" => "MINUTE_5",
        "10m" => "MINUTE_10",
        "15m" => "MINUTE_15",
        "30m" => "MINUTE_30",
        "1h" => "HOUR",
        "2h" => "HOUR_2",
        "3h" => "HOUR_3",
        "4h" => "HOUR_4",
        "1d" => "DAY",
        "1w" => "WEEK",
        "1mo" => "MONTH",
        _ => return None,
    })
}

/// Parse IG's UTC stamp (`snapshotTimeUTC`, `createdDateUTC`, an activity row's `date`: "YYYY-MM-DDTHH:MM:SS",
/// or with a space; a fraction after the seconds is ignored) to epoch ms; 0 on failure.
///
/// ⚠ **A UTC stamp and nothing else.** The account's LOCAL strings (`createdDate` "2026/10/31 03:42:46:000",
/// `snapshotTime` "2026/10/23 23:00:00") are written on the account's own clock (8 hours ahead of UTC on
/// 2026-10-30: `crate::account_clock`) with slashes, and this function used to read them as if they were UTC (it
/// never looked at a separator), so a `createdDate` passed here came back 8 hours late and nothing said so. They
/// are 0 here; `parse_ig_local_wall_clock` (crate-private) reads them for what they are.
pub fn parse_ig_time_utc(s: &str) -> i64 {
    parse_stamp(s, b'-')
}

/// The account's local wall clock ("YYYY/MM/DD HH:MM:SS", what `snapshotTime` carries; a trailing `:000` of a
/// `createdDate` is ignored) as the epoch ms that wall-clock reading would be if it were UTC; 0 on failure.
/// ⚠ **Not an instant**: it is only good as the minuend of a difference from the same row's UTC stamp (the
/// account's offset at it, `klines.rs`'s `read_rows`).
pub(crate) fn parse_ig_local_wall_clock(s: &str) -> i64 {
    parse_stamp(s, b'/')
}

fn parse_stamp(s: &str, date_separator: u8) -> i64 {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != date_separator
        || b[7] != date_separator
        || !matches!(b[10], b'T' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return 0;
    }
    let f = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) =
        (f(0, 4), f(5, 7), f(8, 10), f(11, 13), f(14, 16), f(17, 19))
    else {
        return 0;
    };
    // An out-of-range month/day is as unparseable as any other bad field: a `-1` month wraps to
    // ~4e9 under `as u32` and the day count below then overflows `i64` (a panic in a debug build).
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return 0;
    }
    (vike_util::calendar::days_from_civil(y, mo as u32, d as u32) * 86_400
        + h * 3600
        + mi * 60
        + se)
        * 1000
}

/// Parse an IG `/prices/{epic}` response into mid-price bars.
pub fn parse_prices(v: &serde_json::Value) -> Vec<Bar> {
    let mut out = Vec::new();
    let Some(prices) = v.get("prices").and_then(|p| p.as_array()) else {
        return out;
    };
    for p in prices {
        let mid = |field: &str| -> Option<f64> {
            let o = p.get(field)?;
            let bid = o.get("bid").and_then(serde_json::Value::as_f64)?;
            let ask = o.get("ask").and_then(serde_json::Value::as_f64)?;
            Some((bid + ask) / 2.0)
        };
        let (Some(open), Some(high), Some(low), Some(close)) =
            (mid("openPrice"), mid("highPrice"), mid("lowPrice"), mid("closePrice"))
        else {
            continue;
        };
        let ts = p.get("snapshotTimeUTC").and_then(|t| t.as_str()).map_or(0, parse_ig_time_utc);
        let volume = p.get("lastTradedVolume").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        out.push(Bar {
            ts,
            open,
            high,
            low,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    out
}

/// Fetch up to `max` recent bars for an IG `epic` at `interval` (mid OHLC).
/// `epic` is IG's instrument id (e.g. "CS.D.EURUSD.MINI.IP") — symbol→epic resolution via
/// IG's market search is a follow-up.
pub fn fetch_prices(
    session: &IgSession,
    epic: &str,
    interval: &str,
    max: u32,
) -> Result<Vec<Bar>, IgApiError> {
    let res = resolution(interval).ok_or_else(|| IgApiError {
        status: 0,
        message: format!("unsupported interval {interval}"),
    })?;
    let query = format!("resolution={res}&max={max}&pageSize=0");
    let v = session.get(&format!("/prices/{epic}"), "3", &query)?;
    Ok(parse_prices(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_map() {
        assert_eq!(resolution("1h"), Some("HOUR"));
        assert_eq!(resolution("15m"), Some("MINUTE_15"));
        assert_eq!(resolution("1d"), Some("DAY"));
        assert_eq!(resolution("9s"), None);
    }

    #[test]
    fn time_parse_utc() {
        // 2022-01-15T14:00:00 UTC == 1_642_255_200 s
        assert_eq!(parse_ig_time_utc("2022-01-15T14:00:00"), 1_642_255_200_000);
        assert_eq!(parse_ig_time_utc("1970-01-01T00:00:00"), 0);
        assert_eq!(parse_ig_time_utc("bad"), 0);
    }

    /// **A local stamp is never read as UTC.** A position row on the demo account on 2026-10-30 carried
    /// `createdDate` "2026/10/31 03:42:46:000" beside `createdDateUTC` "2026-10-30T19:42:46" (the account's clock
    /// is 8 hours ahead): the old parse never looked at a separator and read the first as 8 hours after the
    /// second. RED on the old parse (it answered the epoch of 2026-10-31T03:42:46Z).
    #[test]
    fn a_local_wall_clock_is_not_a_utc_stamp() {
        let (local, utc) = ("2026/10/31 03:42:46:000", "2026-10-30T19:42:46");
        assert_eq!(parse_ig_time_utc(local), 0, "the account's local text is not UTC");
        assert!(parse_ig_time_utc(utc) > 0);
        // read for what it is, the local text is the UTC stamp plus the account's offset (480 minutes)
        let offset_ms = parse_ig_local_wall_clock(local) - parse_ig_time_utc(utc);
        assert_eq!(offset_ms, 8 * 3_600_000);
        // and the two readers each refuse the other's text
        assert_eq!(parse_ig_local_wall_clock(utc), 0);
        assert_eq!(parse_ig_local_wall_clock("bad"), 0);
        // a UTC stamp with a space and with a fraction still reads (the activity feed's, the stream's)
        assert_eq!(parse_ig_time_utc("2022-01-15 14:00:00"), 1_642_255_200_000);
        assert_eq!(parse_ig_time_utc("2022-01-15T14:00:00.250"), 1_642_255_200_000);
    }

    #[test]
    fn parse_prices_mid() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"prices": [
                {"snapshotTimeUTC": "2022-01-15T14:00:00", "lastTradedVolume": 123,
                 "openPrice":  {"bid": 1.0900, "ask": 1.0900},
                 "highPrice":  {"bid": 1.0950, "ask": 1.0950},
                 "lowPrice":   {"bid": 1.0890, "ask": 1.0890},
                 "closePrice": {"bid": 1.0930, "ask": 1.0930}}
            ]}"#,
        )
        .unwrap();
        let bars = parse_prices(&v);
        assert_eq!(bars.len(), 1);
        let b = &bars[0];
        assert_eq!(b.ts, 1_642_255_200_000);
        assert_eq!(b.open, 1.09);
        assert_eq!(b.high, 1.095);
        assert_eq!(b.low, 1.089);
        assert_eq!(b.close, 1.093);
        assert_eq!(b.volume, 123.0);
    }
}
