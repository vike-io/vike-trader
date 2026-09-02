//! The `data.vike.io` HOURLY ASSET PANEL — `/v1/{exchange}/assets/hourly` — as a
//! `kind=perp_metrics` producer: one `PerpMetricRow { ts, premium, open_interest }` per hour.
//!
//! # Why this collector exists, stated as the measurement that forced it
//!
//! The panel is the price-context source the dissolved research engine read directly (its
//! `sources/panel.rs`), and it is the ONLY historical source for Hyperliquid open interest —
//! the venue's own `/info` surface serves OI as a current snapshot and nothing else
//! (`crates/vike-data/src/store_kind.rs`'s `perp_metrics` row carries the whole argument). When
//! the cohort study moved onto the store (ADR 0029) its price panel was reassembled from the
//! candle and funding-rate collectors, neither of which carries OI, and the study's 17 OI-derived
//! features silently left the matrix — `oi_share_3xWhale` among them, the column the study's own
//! record names as the size axis's working part. This module is the missing producer.
//!
//! # What is written, and what is deliberately NOT
//!
//! `(ts, premium, open_interest)` becomes a [`PerpMetricRow`], and the decode also carries the
//! panel's `funding_rate` hours as ready `interval=funding` bars ([`PanelRows::funding_bars`])
//! for the caller that OPTS IN (`vikedata_backfill --panel-funding`), and the close hours as
//! close-only `interval=1h` bars for `--panel-bars`. ⚠ The close was CLAIMED equal to the candle
//! series here on a four-hour spot check, and a full-window diff then measured **238 of 2880
//! hours (8.3%) differing by a tick or two** — the vendor's `data_hist_1h` and the venue's
//! `candleSnapshot` are near-identical but distinct series, and `mid_price` feeds the targets,
//! the price context AND the backtest PnL, so the tick-level differences are a different study.
//! The funding rate is the same case louder: the panel's
//! and `fundingHistory`'s funding are DIFFERENT SERIES (snapshot aggregate vs settled interval
//! rate, ~4× apart on measured hours). A store meant to reproduce the dissolved engine's runs
//! takes ALL the panel's halves. ⚠ Whichever halves are taken, one window takes ONE producer per series —
//! the store's idempotency is batch-level on the commit key, two producers are two keys, and the
//! shared hours would land twice.
//!
//! # Decode rules (the old engine's, kept on purpose)
//!
//! * **A null stays a `None`/skip, never a zero.** An hour the panel did not sample is not an
//!   hour whose OI was zero. A null `premium` SKIPS the row (the row contract says a row exists
//!   only for an interval whose premium was reported) and the skip is counted and logged; a null
//!   `open_interest` keeps the row with `None`.
//! * **Every `ts` must be ON THE HOUR** — an off-hour timestamp means the endpoint changed shape,
//!   and the whole fetch is refused rather than rounded.
//! * Timestamps arrive in SECONDS and are stored in MILLISECONDS; the conversion happens here, at
//!   the row boundary, in one place.

use serde::Deserialize;
use vike_data::PerpMetricRow;

use crate::error::CollectError;
use crate::vikedata::client::{fmt_start, API_KEY_HEADER, CTX};
use crate::vikedata::SECS_PER_HOUR;

/// The exact column order the endpoint serves. Positional — a drifted list means the endpoint
/// changed shape, and every cell read below would be silently wrong, so it is checked first.
const PANEL_COLUMNS: [&str; 6] =
    ["ts", "asset", "close", "open_interest", "funding_rate", "premium"];

/// `{base}/{exchange}/assets/hourly?symbols={asset}&start=…&end=…` — the panel URL for ONE asset.
///
/// One asset per fetch, deliberately: the commit key below is per-asset (the store partitions by
/// symbol), and a multi-symbol fetch would need splitting back apart for no saving — the server
/// pages nothing on this endpoint.
pub fn panel_url(
    base: &str,
    exchange: &str,
    asset: &str,
    start_secs: i64,
    end_secs: i64,
) -> String {
    let base = base.trim_end_matches('/');
    format!(
        "{base}/{exchange}/assets/hourly?symbols={asset}&start={}&end={}",
        fmt_start(start_secs),
        fmt_start(end_secs)
    )
}

#[derive(Deserialize)]
struct PanelBody {
    columns: Vec<String>,
    rows: Vec<Vec<serde_json::Value>>,
}

/// The decoded panel: the rows to write plus the null-premium skip count the caller logs.
pub struct PanelRows {
    pub rows: Vec<PerpMetricRow>,
    /// The panel's own funding-rate series, as `interval=funding` bars — the rate on
    /// `Bar::funding`, OHLCV zeroed, exactly `funding_rate_backfill`'s storage shape.
    ///
    /// ⚠ A SECOND funding producer, carried so an operator can CHOOSE a provenance, not so a
    /// store can hold both. The panel's `funding_rate` is the vendor's hourly snapshot aggregate
    /// and `fundingHistory`'s is the venue's settled interval rate — measured ~4× apart on real
    /// hours, so they are different SERIES, not two spellings of one. The dissolved research
    /// engine read the panel's; a store meant to reproduce its runs must too. One store window
    /// takes ONE funding producer — same rule as the premium above.
    pub funding_bars: Vec<vike_model::Bar>,
    /// The panel's close hours as CLOSE-ONLY price bars (`open=high=low=close`, volume `0.0`) for
    /// the `--panel-bars` caller — the module doc's 238-of-2880 measurement is why these exist
    /// beside the candle producer rather than being assumed equal to it. A null close writes no
    /// bar (NaN-not-zero), independently of the other columns' nulls.
    pub close_bars: Vec<vike_model::Bar>,
    pub skipped_null_premium: usize,
}

/// Decode one panel body for one asset. Pure — the fixture-testable seam.
pub fn panel_rows(body: &str, url: &str) -> Result<PanelRows, CollectError> {
    let decoded: PanelBody =
        serde_json::from_str(body).map_err(|e| CollectError::Fetch(format!("{CTX} {url}: {e}")))?;
    if decoded.columns != PANEL_COLUMNS {
        return Err(CollectError::Fetch(format!(
            "{CTX} {url}: columns are {:?}, expected {PANEL_COLUMNS:?} — the endpoint changed \
             shape, and every positional cell read after this point would be silently wrong",
            decoded.columns
        )));
    }
    let cell_f64 = |v: &serde_json::Value| -> Option<f64> { v.as_f64() };
    let mut rows = Vec::with_capacity(decoded.rows.len());
    let mut funding_bars = Vec::with_capacity(decoded.rows.len());
    let mut close_bars = Vec::with_capacity(decoded.rows.len());
    let mut skipped = 0usize;
    for (i, r) in decoded.rows.iter().enumerate() {
        if r.len() != PANEL_COLUMNS.len() {
            return Err(CollectError::Fetch(format!(
                "{CTX} {url}: row {i} has {} cells, expected {} — the endpoint changed shape",
                r.len(),
                PANEL_COLUMNS.len()
            )));
        }
        let ts_secs = r[0].as_i64().ok_or_else(|| {
            CollectError::Fetch(format!("{CTX} {url}: row {i} ts is not an integer"))
        })?;
        if ts_secs % SECS_PER_HOUR != 0 {
            return Err(CollectError::Fetch(format!(
                "{CTX} {url}: row {i} ts {ts_secs} is not on the hour — the endpoint serves \
                 hourly buckets, so an off-hour timestamp means it changed shape"
            )));
        }
        // The close bar first, independent of everything: a null close writes no bar.
        if let Some(close) = cell_f64(&r[2]) {
            close_bars.push(vike_model::Bar {
                ts: ts_secs * 1_000,
                open: close,
                high: close,
                low: close,
                close,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            });
        }
        // The funding bar next, independent of the premium: a null funding hour writes no bar
        // (NaN-not-zero rule), and the premium's own null must not take the rate down with it.
        if let Some(rate) = cell_f64(&r[4]) {
            funding_bars.push(vike_model::Bar {
                ts: ts_secs * 1_000,
                open: 0.0,
                high: 0.0,
                low: 0.0,
                close: 0.0,
                volume: 0.0,
                funding: Some(rate),
                bid: None,
                ask: None,
                symbol: None,
            });
        }
        // A null premium SKIPS the row (the PerpMetricRow contract: a row exists only for an
        // interval whose premium was reported); a null open_interest keeps it with None.
        let Some(premium) = cell_f64(&r[5]) else {
            skipped += 1;
            continue;
        };
        rows.push(PerpMetricRow { ts: ts_secs * 1_000, premium, open_interest: cell_f64(&r[3]) });
    }
    Ok(PanelRows { rows, funding_bars, close_bars, skipped_null_premium: skipped })
}

/// Fetch one asset's panel window and decode it.
pub fn fetch_panel(
    api_key: &str,
    base: &str,
    exchange: &str,
    asset: &str,
    start_secs: i64,
    end_secs: i64,
) -> Result<PanelRows, CollectError> {
    let url = panel_url(base, exchange, asset, start_secs, end_secs);
    let headers = [(API_KEY_HEADER, api_key), ("Accept", "application/json")];
    let opts = crate::http::GetOptions { headers: &headers, ..Default::default() };
    let body = crate::http::get_to_string(&url, &opts, CTX)?;
    panel_rows(&body, &url)
}

/// The commit key one `(exchange, asset, window)` panel ingest writes under.
///
/// ⚠ A DIFFERENT prefix from the funding-rate collector's key on purpose — the two are different
/// producers of the same kind, and the module doc above says why one store must pick one of them.
pub fn panel_commit_key(exchange: &str, asset: &str, start_secs: i64, end_secs: i64) -> String {
    format!("perp_panel:{exchange}:{asset}:{start_secs}-{end_secs}")
}

/// The commit key for the OPT-IN close-bar half — distinct from [`panel_commit_key`] (two
/// series, two keys: sharing one would make whichever half landed second a silent no-op) and
/// from the candle collector's window keys (a different PRODUCER of the same series; the module
/// doc says why a store takes only one).
pub fn panel_bars_commit_key(
    exchange: &str,
    asset: &str,
    start_secs: i64,
    end_secs: i64,
) -> String {
    format!("panel_bars:{exchange}:{asset}:{start_secs}-{end_secs}")
}

/// The funding-bar half's twin of [`panel_bars_commit_key`], distinct from
/// `funding_rate_backfill`'s `funding_rate:` prefix for the same one-producer reason.
pub fn panel_funding_commit_key(
    exchange: &str,
    asset: &str,
    start_secs: i64,
    end_secs: i64,
) -> String {
    format!("panel_funding:{exchange}:{asset}:{start_secs}-{end_secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://data.vike.io/v1/hyperliquid/assets/hourly?symbols=BTC";

    fn body(rows: &str) -> String {
        format!(
            r#"{{"assets":["BTC"],"tz":"UTC","columns":["ts","asset","close","open_interest","funding_rate","premium"],"rows":[{rows}]}}"#
        )
    }

    #[test]
    fn a_row_decodes_to_ms_with_oi_and_premium() {
        let got = panel_rows(&body(r#"[1775538000,"BTC",68628.0,27788.4,2.4e-06,-0.0005]"#), URL)
            .unwrap();
        assert_eq!(got.rows.len(), 1);
        assert_eq!(got.rows[0].ts, 1_775_538_000_000);
        assert_eq!(got.rows[0].premium, -0.0005);
        assert_eq!(got.rows[0].open_interest, Some(27_788.4));
        assert_eq!(got.skipped_null_premium, 0);
    }

    #[test]
    fn a_null_oi_keeps_the_row_and_a_null_premium_skips_it() {
        let got = panel_rows(
            &body(
                r#"[1775538000,"BTC",68628.0,null,2.4e-06,-0.0005],
                   [1775541600,"BTC",68542.0,27873.4,null,null]"#,
            ),
            URL,
        )
        .unwrap();
        assert_eq!(got.rows.len(), 1, "the null-premium row is skipped, not zeroed");
        assert_eq!(got.rows[0].open_interest, None, "a null OI is None, never 0.0");
        assert_eq!(got.skipped_null_premium, 1);
    }

    #[test]
    fn a_drifted_column_list_and_an_off_hour_ts_are_both_refused() {
        let drifted = r#"{"columns":["ts","asset","close"],"rows":[]}"#;
        assert!(panel_rows(drifted, URL).is_err(), "a drifted column list must refuse");
        let off_hour = body(r#"[1775538001,"BTC",68628.0,1.0,2.4e-06,-0.0005]"#);
        assert!(panel_rows(&off_hour, URL).is_err(), "an off-hour ts must refuse");
    }

    #[test]
    fn the_url_and_key_spell_the_window_the_same_way() {
        assert_eq!(
            panel_url(
                "https://data.vike.io/v1/",
                "hyperliquid",
                "BTC",
                1_775_538_000,
                1_775_541_600
            ),
            "https://data.vike.io/v1/hyperliquid/assets/hourly?symbols=BTC\
             &start=2026-04-07T05:00:00Z&end=2026-04-07T06:00:00Z"
        );
        assert_eq!(
            panel_commit_key("hyperliquid", "BTC", 1_775_538_000, 1_775_541_600),
            "perp_panel:hyperliquid:BTC:1775538000-1775541600"
        );
    }
}
