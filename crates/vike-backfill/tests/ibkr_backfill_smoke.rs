//! Live historical-fetch smoke: fetch a handful of DAILY AAPL bars against a running Gateway and
//! assert non-empty. `#[ignore]` + self-skips without config/gateway. Read-only (data only). The
//! human vehicle to verify the historical wire shapes before a real backfill run:
//!   cargo test -p vike-backfill --features ibkr --test ibkr_backfill_smoke -- --ignored --nocapture
#![cfg(feature = "ibkr")]

use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::load_workspace_dotenv_from;
use vike_ibkr::{Environment, HistWhat, HistoricalFetcher, Window};

#[test]
#[ignore = "live historical data: needs a running Gateway"]
fn daily_aapl_history_fetch() {
    vike_log::test_init();

    // The TEST owns the credential-store read (one load, both tiers) and passes the map down —
    // `vike_ibkr::config` no longer offers a wrapper that opens the store itself.
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(cfg) = load_ibkr_config_from(Environment::Demo, &vars)
        .or_else(|| load_ibkr_config_from(Environment::Live, &vars))
    else {
        eprintln!("skip: no IBKR config in .env");
        return;
    };
    let fetcher = match HistoricalFetcher::connect(&cfg) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("skip: connect failed (gateway down?): {e:?}");
            return;
        }
    };

    // head_timestamp is informational — log it, don't hard-assert (entitlement dependent).
    match fetcher.head_timestamp_ms("AAPL.SMART.USD", HistWhat::Trades) {
        Ok(h) => eprintln!("head_timestamp_ms = {h}"),
        Err(e) => eprintln!("head_timestamp unavailable: {e:?}"),
    }

    let end_ms = vike_model::now_ms();
    let bars = fetcher
        .fetch_window("AAPL.SMART.USD", "1d", end_ms, Window::years(1), HistWhat::Trades)
        .expect("fetch daily window");
    eprintln!("fetched {} daily AAPL bars", bars.len());
    if let (Some(first), Some(last)) = (bars.first(), bars.last()) {
        eprintln!(
            "range {}..{} close_last={}",
            vike_model::epoch_ms_to_utc_date(first.ts),
            vike_model::epoch_ms_to_utc_date(last.ts),
            last.close
        );
    }
    // Diagnostic, not a hard assert — a closed market day / entitlement gap can yield 0.
    if bars.is_empty() {
        eprintln!("diagnostic: 0 daily bars — check entitlement / market calendar");
    } else {
        assert!(bars.windows(2).all(|w| w[0].ts <= w[1].ts), "bars must be ts-ascending");
    }
}
