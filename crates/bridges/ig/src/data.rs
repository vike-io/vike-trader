//! IG prices → [`Bar`]. IG quotes each OHLC point as a `{bid, ask}` object, so bars are built
//! from the MID. Timestamps come as `snapshotTimeUTC` ("YYYY-MM-DDTHH:MM:SS") — parsed to epoch-ms
//! with a small civil-date routine (no chrono dependency).

use crate::rest::{IgApiError, IgSession};
use vike_model::Bar;

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

/// Parse IG's `snapshotTimeUTC` ("YYYY-MM-DDTHH:MM:SS", or with a space) to epoch ms; 0 on failure.
pub fn parse_ig_time_utc(s: &str) -> i64 {
    if s.len() < 19 {
        return 0;
    }
    let f = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) =
        (f(0, 4), f(5, 7), f(8, 10), f(11, 13), f(14, 16), f(17, 19))
    else {
        return 0;
    };
    (vike_model::time::days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + se)
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
