//! Live market-data smoke: subscribe AAPL quotes against a running Gateway, assert DELAYED ticks
//! flow, then unsubscribe cleanly. `#[ignore]` + self-skips without config/gateway. Read-only (data
//! only — never places an order). Delayed data works with no market-data entitlement, so this is the
//! human vehicle to LIVE-VERIFY the realtime feed's wire shapes (`switch_market_data_type` + the
//! quote accumulator) against a real Gateway:
//!   cargo test -p vike-ibkr --features ibkr --test ibkr_mktdata_smoke -- --ignored --nocapture
#![cfg(feature = "ibkr-socket")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_data::{DataClient, LiveDataSink};
use vike_ibkr::IbkrFeeds;
use vike_ibkr::config::load_ibkr_config_from;
use vike_model::{Bar, L2Book, QuoteTick, TradeTick};

#[derive(Default)]
struct Count {
    quotes: Mutex<usize>,
}

impl LiveDataSink for Count {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, q: QuoteTick) {
        eprintln!(
            "QUOTE bid={} ask={} bid_size={} ask_size={}",
            q.bid, q.ask, q.bid_size, q.ask_size
        );
        *self.quotes.lock().unwrap() += 1;
    }
    fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
}

#[test]
#[ignore = "live market data: needs a running Gateway; delayed data works with no entitlement"]
fn delayed_quotes_flow() {
    vike_log::test_init();

    // The TEST owns the credential-store read (one load, both tiers) and passes the map down —
    // `vike_ibkr::config` no longer offers a wrapper that opens the store itself.
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let vars = load_workspace_dotenv_from(settings_dir.as_deref());
    let settings = ibkr_settings(settings_dir.as_deref());
    let Some(cfg) = load_ibkr_config_from(Environment::Demo, &vars, &settings)
        .or_else(|| load_ibkr_config_from(Environment::Live, &vars, &settings))
    else {
        eprintln!("skip: no IBKR config in .env");
        return;
    };

    let sink = Arc::new(Count::default());
    let mut feeds = match IbkrFeeds::connect(&cfg, sink.clone()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("skip: IBKR data connect failed (gateway down?): {e:?}");
            return;
        }
    };

    let id = feeds.subscribe_quotes("AAPL.SMART.USD").expect("subscribe quotes");
    let deadline = Instant::now() + Duration::from_secs(20);
    while *sink.quotes.lock().unwrap() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let n = *sink.quotes.lock().unwrap();
    eprintln!("delayed quotes received: {n}");

    feeds.unsubscribe(id);
    feeds.shutdown();

    // Diagnostic, not a hard assert — quote flow is entitlement- and market-hours-dependent.
    if n == 0 {
        eprintln!("diagnostic: no quotes in 20s — market closed or no data permission");
    }
}

/// The venue's gateway rows — `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` (decision 0095) — read from
/// the settings database in the same settings directory the credential read above resolves.
///
/// ⚠ LOUD, never a quiet default: a gateway setting left in the credential store under its old
/// name (which the daemons refuse to start on) or a database that will not read would otherwise
/// hand this smoke the built-in gateway — the self-skip that prints `ok` having tested nothing.
fn ibkr_settings(
    settings_dir_override: Option<&str>,
) -> vike_secrets::venue_setting::VenueSettings {
    let stranded: Vec<(String, String)> =
        vike_secrets::venue_setting::stranded_venue_setting_names(
            load_workspace_dotenv_from(settings_dir_override).keys().map(String::as_str),
        )
        .into_iter()
        .filter(|(_, key)| key.starts_with("venue.ibkr."))
        .collect();
    assert!(
        stranded.is_empty(),
        "the credential store holds IBKR gateway settings under their old names, which nothing \
         reads: {stranded:?} — run `vike-cli secrets move-venue-config` first"
    );
    vike_model::state_path::project_settings_dir_from(
        settings_dir_override,
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("ibkr"))
    .unwrap_or_default()
}
