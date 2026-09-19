//! Tardis datasets transport: build the per-day `.csv.gz` URL, authenticate with a Bearer token
//! (optional — first-of-month is a free keyless sample), download + gzip-decompress one day's file.
//! A missing day (HTTP 404) is `Ok(None)`, not an error (mirrors `pmxt::download_hour`).

use std::io::Read;

use flate2::read::GzDecoder;

use crate::error::CollectError;

pub const DATASETS_BASE: &str = "https://datasets.tardis.dev/v1";

/// `https://datasets.tardis.dev/v1/{exchange}/{data_type}/{YYYY}/{MM}/{DD}/{symbol}.csv.gz`
pub fn day_url(exchange: &str, data_type: &str, y: i32, m: u32, d: u32, symbol: &str) -> String {
    format!("{DATASETS_BASE}/{exchange}/{data_type}/{y:04}/{m:02}/{d:02}/{symbol}.csv.gz")
}

/// Download one day's `.csv.gz`, gzip-decompress to a CSV `String`. `Ok(None)` on 404 (day absent).
/// `api_key` = `None` uses the keyless free tier (first-of-month only). Non-2xx (≠404) or a
/// transport/decode failure → `Err`.
pub fn fetch_day(
    api_key: Option<&str>,
    exchange: &str,
    data_type: &str,
    y: i32,
    m: u32,
    d: u32,
    symbol: &str,
) -> Result<Option<String>, CollectError> {
    let url = day_url(exchange, data_type, y, m, d, symbol);
    let auth = api_key.map(|key| format!("Bearer {key}"));
    let opts = crate::http::GetOptions { authorization: auth.as_deref(), ..Default::default() };
    // Read the gzip body fully (one day's file is bounded); `Ok(None)` on 404 (day absent).
    let Some(gz) = crate::http::get_to_bytes(&url, &opts, "tardis")? else {
        return Ok(None);
    };
    let mut csv = String::new();
    GzDecoder::new(&gz[..])
        .read_to_string(&mut csv)
        .map_err(|e| CollectError::Fetch(format!("gunzip tardis body {url}: {e}")))?;
    Ok(Some(csv))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_url_zero_pads_and_orders_path() {
        assert_eq!(
            day_url("deribit", "trades", 2024, 3, 7, "BTC-PERPETUAL"),
            "https://datasets.tardis.dev/v1/deribit/trades/2024/03/07/BTC-PERPETUAL.csv.gz"
        );
    }
}
