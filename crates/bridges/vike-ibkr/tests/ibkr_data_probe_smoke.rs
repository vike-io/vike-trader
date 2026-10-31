//! LIVE PAPER, READ-ONLY probe of two facts of the socket gateway that the price lane depends on (decision 0220).
//! It places NO order, writes nothing and changes no setting: it is one more API client of the paper gateway under
//! ids of its own (30000-39999 by the UTC day, the slots the other smokes leave free), and each section prints the
//! line a human reads.
//!
//! | section | question | what it prints |
//! |---|---|---|
//! | TYPE | does a connection that does NOT send `reqMarketDataType` (a session `ibapi` rebuilt after a socket loss is one) get `[354]` for a stock, and does the same connection stream once it does? | the first answer to each of the two subscribes, with the seconds |
//! | RULE | what does IB's market rule of `SAP.SMART.EUR` say, and what grid tick do the production facts take from it? | every band of every rule the contract names, the facts' tick, and the first delayed quote of the stock |
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr,test-support --test ibkr_data_probe_smoke -- --ignored --nocapture
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

mod common;
use common::ibkr_settings;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::client::blocking::Client;
use ibapi::contracts::{Contract, Currency, Exchange, SecurityType, Symbol};
use ibapi::market_data::MarketDataType;
use ibapi::subscriptions::SubscriptionItem;
use vike_ibkr::any_symbol_socket::IbkrFactsSource;
use vike_ibkr::config::{IbkrConfig, load_ibkr_config_from};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::IbkrBackend;

/// The ids this probe may use: two of the UTC day's eight (`ibkr_delayed_price_smoke` takes the first three).
fn todays_ids() -> [i32; 2] {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86_400;
    let base = 30_000 + (days % 1_000) as i32 * 8;
    [base + 5, base + 6]
}

fn live_cfg() -> Option<(IbkrConfig, String)> {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(mut cfg) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return None;
    };
    if cfg.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; this probe speaks the IB Gateway socket"
        );
        return None;
    }
    assert!(
        cfg.account.starts_with("DU"),
        "the probe must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    let [client, data] = todays_ids();
    (cfg.client_id, cfg.data_client_id) = (client, data);
    let addr = format!("{}:{}", cfg.host, cfg.port);
    Some((cfg, addr))
}

fn stock(symbol: &str, exchange: &str, currency: &str) -> Contract {
    Contract {
        symbol: Symbol::from(symbol),
        security_type: SecurityType::Stock,
        exchange: Exchange::from(exchange),
        currency: Currency::from(currency),
        ..Contract::default()
    }
}

/// The first answer to a streaming market-data subscribe of `contract`, as one line, within `within`.
fn first_answer(client: &Client, contract: &Contract, within: Duration) -> String {
    let began = Instant::now();
    let sub = match client.market_data(contract).streaming().subscribe() {
        Ok(sub) => sub,
        Err(e) => return format!("the subscribe itself was refused: {e}"),
    };
    let mut notices = Vec::new();
    while began.elapsed() < within {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(tick))) => {
                return format!(
                    "first tick after {:.1} s: {tick:?} (advisories before it: {notices:?})",
                    began.elapsed().as_secs_f64()
                );
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                notices.push(format!("[{}] {}", n.code, n.message))
            }
            Some(Err(e)) => {
                return format!(
                    "the stream ENDED after {:.1} s: {e}",
                    began.elapsed().as_secs_f64()
                );
            }
            None => {}
        }
    }
    format!("nothing within {} s (advisories: {notices:?})", within.as_secs())
}

/// READ-ONLY. See the module doc.
#[test]
#[ignore = "live paper: one more API client for about a minute; places nothing"]
fn ibkr_data_probe_observe_only() {
    vike_log::test_init();
    let Some((cfg, addr)) = live_cfg() else { return };
    eprintln!(
        "gateway {addr}, paper account DU\u{2026}, client ids {:?}; nothing is placed",
        todays_ids()
    );

    // ---- TYPE: a connection with no market-data type sent, then the same one with it ----
    let client = Client::connect(&addr, cfg.client_id).expect("the probe connects");
    let msft = stock("MSFT", "SMART", "USD");
    eprintln!(
        "[TYPE] 1 no reqMarketDataType sent (the state of a session ibapi rebuilt): MSFT streaming reqMktData"
    );
    eprintln!("[TYPE]   -> {}", first_answer(&client, &msft, Duration::from_secs(10)));
    client.switch_market_data_type(MarketDataType::Delayed).expect("the type is sent");
    eprintln!(
        "[TYPE] 2 reqMarketDataType(delayed) sent on the SAME connection, MSFT subscribed again"
    );
    eprintln!("[TYPE]   -> {}", first_answer(&client, &msft, Duration::from_secs(20)));

    // ---- RULE: every band of every market rule of SAP.SMART.EUR, and the grid tick the production facts take ----
    let sap = stock("SAP", "SMART", "EUR");
    match client.contract_details(&sap) {
        Ok(rows) => {
            let mut ids: Vec<i32> = Vec::new();
            for row in &rows {
                eprintln!(
                    "[RULE] contract conId={} primary={} min_tick={} rule ids={:?} valid exchanges={}",
                    row.contract.contract_id,
                    row.contract.primary_exchange.0,
                    row.min_tick,
                    row.market_rule_ids,
                    row.valid_exchanges.join(",")
                );
                for word in row.market_rule_ids.iter().flat_map(|w| w.split(',')) {
                    if let Ok(id) = word.trim().parse::<i32>()
                        && !ids.contains(&id)
                    {
                        ids.push(id);
                    }
                }
            }
            for id in ids {
                match client.market_rule(id) {
                    Ok(rule) => {
                        let bands: Vec<String> = rule
                            .price_increments
                            .iter()
                            .map(|b| format!("{}:{}", b.low_edge, b.increment))
                            .collect();
                        eprintln!(
                            "[RULE] market rule {id}: low_edge:increment = {}",
                            bands.join(" ")
                        );
                    }
                    Err(e) => eprintln!("[RULE] market rule {id}: {e}"),
                }
            }
        }
        Err(e) => eprintln!("[RULE] contract details of SAP.SMART.EUR: {e}"),
    }
    drop(client);

    let facts = IbkrFactsSource::new(cfg.clone()).facts_any_currency("SAP.SMART.EUR");
    eprintln!("[RULE] production facts of SAP.SMART.EUR: {facts:?}");
    let client = Client::connect(&addr, cfg.client_id).expect("the probe connects again");
    client.switch_market_data_type(MarketDataType::Delayed).expect("the type is sent");
    eprintln!(
        "[RULE] SAP.SMART.EUR first delayed quote -> {}",
        first_answer(&client, &sap, Duration::from_secs(20))
    );
}
