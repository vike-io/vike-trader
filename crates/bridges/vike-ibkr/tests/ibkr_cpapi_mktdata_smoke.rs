//! Live READ-ONLY market-data smoke for the **cpapi** backend (decision 0140): reads one forex pair
//! and one stock through a running, browser-authenticated Client Portal Gateway on the DEMO (paper)
//! account, and checks the delayed label against the owner's stated entitlements — forex
//! REAL-TIME, stocks DELAYED. Never places an order. Double-gated: `#[ignore]` AND self-skips
//! without a demo ibkr account holding an `ACCOUNT` (the default label) or a reachable,
//! authenticated gateway. Run explicitly:
//!   cargo test -p vike-ibkr --features ibkr --test ibkr_cpapi_mktdata_smoke -- --ignored --nocapture
//!
//! This is the vehicle that retires the UNVERIFIED wire shapes in
//! `crates/bridges/vike-ibkr/src/cpapi_feed/decode.rs` (the `smd` field codes, the `6509`
//! availability letters, the snapshot and its first-request preflight) and in
//! `crates/bridges/vike-ibkr/src/transport/cpapi/endpoints.rs`'s two market-data wrappers: it PRINTS
//! every raw row it reads, so a mismatch is corrected from this output. It also prints which pair
//! `secdef/search` resolved `EUR` to — the first row of a base-currency search, which nothing has
//! observed yet.
//!
//! The second half drives the real client through the one door
//! (`vike_ibkr::connect_data_client`) and counts quotes for 20 s — a diagnostic, not an assertion:
//! a quote's arrival depends on the market being open.
#![cfg(feature = "ibkr-cpapi")]

mod common;
use common::ibkr_settings;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::contract::parse_simplified;
use vike_ibkr::cpapi_feed::decode::{SmdAccumulator, snapshot_fields, snapshot_row};
use vike_ibkr::transport::CpapiRest;
use vike_ibkr::transport::cpapi_decode::decode_conid;
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, DataDelay};
use vike_venues::IbkrBackend;

/// (canonical symbol, the label the owner's entitlement table says it arrives with).
const READS: [(&str, DataDelay); 2] =
    [("EUR.USD.IDEALPRO", DataDelay::RealTime), ("AAPL.SMART.USD", DataDelay::DELAYED)];

#[derive(Default)]
struct Count {
    quotes: Mutex<Vec<String>>,
    delays: Mutex<Vec<(String, DataDelay)>>,
}

impl LiveDataSink for Count {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, symbol: &str, q: QuoteTick) {
        eprintln!("QUOTE {symbol} bid={} ask={} {}x{}", q.bid, q.ask, q.bid_size, q.ask_size);
        self.quotes.lock().unwrap().push(symbol.to_string());
    }
    fn trade(&self, _v: &str, symbol: &str, t: TradeTick) {
        eprintln!("TRADE {symbol} {}@{}", t.size, t.price);
    }
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
    fn data_delay(&self, _v: &str, symbol: &str, stream: &str, delay: DataDelay) {
        eprintln!("DELAY {symbol} {stream} {}", delay.word());
        self.delays.lock().unwrap().push((symbol.to_string(), delay));
    }
}

#[test]
#[ignore = "live market data: needs a running, authenticated demo CP Gateway; read-only"]
fn cpapi_forex_real_time_and_stock_delayed() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(mut cfg) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT`");
        return;
    };
    cfg.backend = IbkrBackend::Cpapi;

    // Read-only, but still the DEMO gateway: cpapi answers for whoever is logged into the gateway,
    // so the session account (not `cfg.account`) is what proves which account this reads.
    let rest = CpapiRest::new(&cfg.cpapi_url, &cfg.account);
    let session = match rest.accounts() {
        Ok(v) => v["accounts"][0].as_str().unwrap_or_default().to_string(),
        Err(e) => {
            eprintln!(
                "skip: /iserver/accounts unreadable (gateway down or unauthenticated): {e:?}"
            );
            return;
        }
    };
    assert!(
        session.starts_with("DU"),
        "the gateway session is on '{session}', not a paper DU… account — this smoke reads the DEMO \
         session only"
    );
    eprintln!("session account: {session}");

    for (symbol, expected) in READS {
        let contract = parse_simplified(symbol).expect("a canonical this crate reads");
        let search = rest
            .secdef_search(&contract.symbol, contract.sec_type.as_ib_code())
            .unwrap_or_else(|e| panic!("secdef_search {symbol}: {e:?}"));
        eprintln!("SECDEF {symbol}: first row {}", search.get(0).cloned().unwrap_or_default());
        let conid = decode_conid(&search).unwrap_or_else(|| panic!("no conId for {symbol}"));

        // The first snapshot request for a conId only primes it (IBKR's docs): ask until a row
        // carries data, a bounded number of times.
        let mut row = None;
        for attempt in 0..6 {
            let body = rest
                .marketdata_snapshot(conid, &snapshot_fields())
                .unwrap_or_else(|e| panic!("snapshot {symbol}: {e:?}"));
            eprintln!("SNAPSHOT {symbol} attempt {attempt}: {body}");
            if let Some(r) = snapshot_row(&body, conid) {
                row = Some(r.clone());
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let row = row.unwrap_or_else(|| panic!("{symbol}: no snapshot row ever carried data"));
        let delay = SmdAccumulator::default().apply(&row, vike_model::now_ms()).delay;
        eprintln!("{symbol}: availability {delay:?}");
        let Some(delay) = delay else {
            panic!(
                "{symbol}: the row carried no `6509` marker the decoder reads (or `N`, no \
                 subscription) — correct `decode.rs` from this row: {row}"
            )
        };
        if delay == DataDelay::Frozen {
            // Frozen and delayed-frozen read alike, so a closed market decides nothing here.
            eprintln!(
                "{symbol}: FROZEN (market closed?) — this run cannot tell real-time from delayed; \
                 rerun in trading hours for a decisive read"
            );
            continue;
        }
        assert_eq!(
            delay,
            expected,
            "{symbol}: the gateway labels it {}, the entitlement table says {} (row: {row})",
            delay.word(),
            expected.word()
        );
    }

    // The client itself, through the one door: quotes for both symbols, for 20 s.
    let sink = Arc::new(Count::default());
    let mut client = match vike_ibkr::connect_data_client(&cfg, sink.clone()) {
        Ok(c) => c,
        Err(e) => {
            panic!("the cpapi data client did not open on a gateway that just answered: {e:?}")
        }
    };
    let ids: Vec<_> = READS
        .iter()
        .map(|(s, _)| client.subscribe_quotes(s).unwrap_or_else(|e| panic!("subscribe {s}: {e}")))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let got = sink.quotes.lock().unwrap().clone();
        if READS.iter().all(|(s, _)| got.iter().any(|g| g == s)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let got = sink.quotes.lock().unwrap().clone();
    for (s, _) in READS {
        eprintln!("{s}: {} quotes over the stream", got.iter().filter(|g| g.as_str() == s).count());
    }
    // The streams' own labels (`LiveDataSink::data_delay`, sent at first knowledge and on change
    // only): printed, a diagnostic like the counts above.
    for (s, d) in sink.delays.lock().unwrap().iter() {
        eprintln!("{s}: stream labelled {}", d.word());
    }
    for id in ids {
        client.unsubscribe(id);
    }
    client.shutdown();
}
