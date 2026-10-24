//! LIVE read-only smokes for the IBKR multi-currency rate source (decision 0183, `vike_ibkr::fx_rate`). They
//! settle what the offline tests can only describe: whether the paper account's `NetLiquidation` row states its
//! base currency, whether a real-time forex quote streams to it, which orientation IBKR lists a pair in, how the
//! rate compares with the account's own `ExchangeRate` ledger row, and what IBKR books for a round trip in a
//! foreign-quoted stock (the venue's own `realizedPNL`, the cash it moved per currency, the currency it charged
//! the commission in). The owner runs them from a the CI box lane (`docs/ops/live-smoke-lane.md`), before and after the
//! drill in `RUNBOOK-ibkr-currency.md`:
//!
//!     cargo test -p vike-ibkr --features ibkr --test ibkr_fx_smoke -- --ignored --nocapture
//!
//! READ-ONLY: places NO order and never touches the account. Triple-gated: `#[ignore]`; self-skips unless a demo
//! ibkr account holding an `ACCOUNT`, on the `socket` backend, exists and the gateway answers; and a hard assert
//! refuses anything but a paper `DU…` account. `ibkr_fx_rate_smoke` reads through the gateway's DATA connection
//! (`IbkrFactsSource`) as ONE ask, so the whole smoke spends ONE dial of the data lane's budget: the gate allows
//! six data connects an hour per gateway and a lookup caller may spend at most four of them. (It used to read
//! with a connection per read, about ten dials, so its fifth and later were REFUSED by that cap, and a refused
//! dial is not an answer about the account.) Run it once, not in a loop.
//!
//! ⚠ IBKR allows a SESSION (one TCP connection) only TWO account-summary requests in all, a cancelled one included
//! (measured 2026-10-20, `crates/bridges/vike-ibkr/src/recon/summary.rs`'s module doc). Each connection has its OWN
//! count: the base-currency read rides the gateway's data connection (`data_client_id`), `ibkr_fx_figures_smoke` its own
//! API client (below), and the reconcile client's kept `$LEDGER:ALL` subscription is on the `recon_client_id`
//! connection, so none of the three spends another's. Run each smoke once, not in a loop: a third request on one
//! connection is refused until a new connection, and surfaces as `ibkr: the account summary: ...`; read its exact code.
//! Whether a request on ANOTHER connection counts against a session was not measured.
//!
//! `ibkr_fx_figures_smoke` opens its own API client under `client_id + 20`, a number nothing else in this tree
//! derives (the exec holds `client_id`, the data lane `data_client_id`, the reconcile client `recon_client_id`).
//! ⚠ IB may show a non-master client id only ITS OWN executions (`src/recon/socket.rs`, open question 1), in which
//! case this smoke lists no execution of the exec client's: the ledger rows are still read, and the venue's
//! `realizedPNL` of the drill's closing fill is then read in TWS (Trade Log) by hand.
#![cfg(feature = "ibkr")]

mod common;
use common::ibkr_settings;

use std::time::{Duration, Instant};

use ibapi::accounts::types::AccountGroup;
use ibapi::accounts::{AccountSummaryResult, AccountSummaryTags};
use ibapi::client::blocking::Client;
use ibapi::orders::{ExecutionFilter, Executions};
use ibapi::subscriptions::SubscriptionItem;
use vike_ibkr::IbkrConfig;
use vike_ibkr::any_symbol::FactsError;
use vike_ibkr::any_symbol_socket::IbkrFactsSource;
use vike_ibkr::config::load_ibkr_config_from;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::IbkrBackend;

/// The ledger tags worth printing from `$LEDGER:ALL` (IB's documented names, not an observation until this runs).
const LEDGER_TAGS: &[&str] = &[
    "Currency",
    "CashBalance",
    "TotalCashBalance",
    "ExchangeRate",
    "StockMarketValue",
    "NetLiquidationByCurrency",
    "RealizedPnL",
    "UnrealizedPnL",
];

/// The paper account's config, or `None` after saying why this smoke skips.
fn paper_config() -> Option<IbkrConfig> {
    vike_log::test_init();
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(cfg) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return None;
    };
    if cfg.backend == IbkrBackend::Cpapi {
        eprintln!("skip: venue.ibkr.demo.backend is cpapi; these smokes read the IB Gateway");
        return None;
    }
    // HARD SAFETY: paper only. This assert stays a hard failure, never a skip.
    assert!(
        cfg.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    eprintln!("gateway {}:{} data client id {}", cfg.host, cfg.port, cfg.data_client_id);
    Some(cfg)
}

/// The band, in USD per 1 `ccy`, a rate for `ccy` must sit in, in 2026 terms: wide enough for any plausible move,
/// narrow enough that the flipped rate (yen per dollar, `150` for a yen) is far outside it. Applied on a USD
/// ledger as it stands and on any other through the base's USD rate ([`direction_check`]).
fn usd_band(ccy: &str) -> Option<(f64, f64)> {
    Some(match ccy {
        "EUR" => (0.7, 1.6),
        "JPY" => (0.003, 0.02),
        "CAD" => (0.6, 0.9),
        "CHF" => (0.8, 1.4),
        "SEK" => (0.07, 0.15),
        "AUD" => (0.5, 0.9),
        _ => return None,
    })
}

/// **The direction check, for EVERY currency the smoke reads and whatever the account's base** (V1 of the vendor-doc
/// verification: IBKR's pages never state which way a forex pair is priced). The rate is put in USD terms through
/// the base's own USD rate read in the same ask (`usd_in_base`: base units per 1 USD, `1.0` on a USD ledger) and
/// printed with the pair as listed, so a person can compare each line with the same pair in TWS; where a band is
/// known ([`usd_band`]) it is also asserted. A flipped rate is outside every band by two orders of magnitude.
fn direction_check(
    base: &str,
    ccy: &str,
    per_quote: f64,
    pair: &str,
    usd_in_base: Option<f64>,
) -> Result<(), String> {
    let Some(usd_in_base) = usd_in_base else {
        eprintln!(
            "direction check {ccy}: SKIPPED, the {base} value of 1 USD was not read, so this rate cannot be put \
             in USD terms (the pair as listed is {pair}, {per_quote} {base} per 1 {ccy}): compare with TWS by eye"
        );
        return Ok(());
    };
    let usd_per_ccy = per_quote / usd_in_base;
    match usd_band(ccy) {
        Some((low, high)) => {
            eprintln!(
                "direction check {ccy}: {usd_per_ccy:.6} USD per 1 {ccy}, band {low}..{high} (the pair as listed is {pair})"
            );
            if !(low..=high).contains(&usd_per_ccy) {
                return Err(format!(
                    "{ccy}: {usd_per_ccy} USD per {ccy} (from {pair}, {per_quote} {base} per {ccy}) is outside \
                     {low}..{high}: a FLIPPED rate (the 22,500x bug) or a stale/empty market"
                ));
            }
        }
        None => eprintln!(
            "direction check {ccy}: {usd_per_ccy:.6} USD per 1 {ccy}, no band known for {ccy}: compare with TWS by eye \
             (the pair as listed is {pair})"
        ),
    }
    Ok(())
}

/// What a read that did not answer MEANS, in the words the drill reads. A dial the data lane's gate refused
/// (`Capped`: the hour's budget is spent; `Spaced`: its backoff runs; "nothing was dialled") says NOTHING about
/// the account. Only a forex quote that ARRIVED and is not real-time says the account has no real-time forex
/// entitlement.
fn why_not(e: &FactsError) -> String {
    match e {
        FactsError::Refused(why) => format!("REFUSED by name or by the market: {why}"),
        FactsError::Unavailable(why) if why.contains("nothing was dialled") => format!(
            "NOT READ: the data lane's gate refused the dial (budget or backoff), which says nothing \
             about the account's entitlement: {why}"
        ),
        FactsError::Unavailable(why) if why.contains("not real-time") => {
            format!("NO REAL-TIME FOREX ENTITLEMENT (a quote arrived, delayed or frozen): {why}")
        }
        FactsError::Unavailable(why) => {
            format!("NOT READ (neither a refused dial nor a delayed quote): {why}")
        }
    }
}

#[test]
#[ignore = "network + a running IB Gateway logged into a paper DU… account — run manually (see module doc)"]
fn ibkr_fx_rate_smoke() {
    let Some(cfg) = paper_config() else { return };
    let source = IbkrFactsSource::new(cfg);
    // ONE ask for the whole smoke: the base currency, every rate and both stock lookups ride one connection,
    // so the smoke dials once (see the module doc).
    let ask = source.scope();

    let base = source
        .account_base_currency_in(&ask)
        .unwrap_or_else(|e| panic!("the account's base currency: {}", why_not(&e)));
    eprintln!("base currency of the account (NetLiquidation row): {base}");
    assert!(base.len() == 3 && base.bytes().all(|b| b.is_ascii_uppercase()), "{base}");

    // What 1 USD is worth in the account's own money: the factor that puts every other rate in USD terms for the
    // direction check, whatever the base (`1.0` on a USD ledger). Read first, once; the loop's own USD line is
    // then served from the source's keep, with no second read.
    let usd_in_base = if base == "USD" {
        Some(1.0)
    } else {
        match source.rate_for_in(&ask, "USD") {
            Ok(answer) => answer.rate.map(|rate| rate.account_per_quote.get()),
            Err(e) => {
                eprintln!("USD: no rate: {}", why_not(&e));
                None
            }
        }
    };
    let mut wrong_way: Vec<String> = Vec::new();
    for ccy in ["EUR", "JPY", "CAD", "CHF", "SEK", "AUD", "USD"] {
        match source.rate_for_in(&ask, ccy) {
            Ok(answer) => {
                assert_eq!(answer.base, base);
                match answer.rate {
                    None => {
                        eprintln!("{ccy}: is the base currency, no conversion");
                        assert_eq!(ccy, base);
                    }
                    Some(rate) => {
                        let per_quote = rate.account_per_quote.get();
                        eprintln!(
                            "{ccy}: {per_quote} {base} per 1 {ccy} (from the pair {}, asked at {})",
                            rate.pair, rate.as_of_ms
                        );
                        assert!(per_quote.is_finite() && per_quote > 0.0, "{ccy}: {per_quote}");
                        assert_eq!(rate.quote_ccy, ccy);
                        if let Err(why) =
                            direction_check(&base, ccy, per_quote, &rate.pair, usd_in_base)
                        {
                            wrong_way.push(why);
                        }
                    }
                }
            }
            // A rate that cannot be had is an answer too, but WHICH answer is the whole point: a closed market, a
            // pair that is quiet, a delayed quote (no real-time forex entitlement) or a dial the gate refused
            // are four different findings ([`why_not`]). Print it, it is what the drill's first step has to know.
            Err(e) => eprintln!("{ccy}: no rate: {}", why_not(&e)),
        }
    }
    // Every currency was printed before the verdict, so a drill reads all of them: any wrong direction fails here.
    assert!(wrong_way.is_empty(), "the direction check failed: {wrong_way:#?}");
    // A minor-unit currency is refused by name before any dial.
    let gbp = source.rate_for_in(&ask, "GBP").expect_err("GBP is refused by name");
    eprintln!("GBP: {}", why_not(&gbp));

    // The stock lookup without the currency fence: one EUR and one JPY name. Printed, not asserted: a ticker's
    // listing (ambiguous on two exchanges, or not listed under that ticker) is the very thing being learned.
    for name in ["SAP.SMART.EUR", "7203.SMART.JPY"] {
        match source.facts_any_currency_in(&ask, name) {
            Ok(facts) => eprintln!("{name}: {facts:?}"),
            Err(e) => eprintln!("{name}: {}", why_not(&e)),
        }
    }
    eprintln!("IBKR fx rate smoke done: read-only, account untouched");
}

#[test]
#[ignore = "network + a running IB Gateway logged into a paper DU… account — run manually (see module doc)"]
fn ibkr_fx_figures_smoke() {
    let Some(cfg) = paper_config() else { return };
    let id = cfg.client_id + 20;
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let client = match Client::connect(&addr, id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: could not connect to {addr} under client id {id}: {e}");
            return;
        }
    };
    eprintln!("connected under client id {id}");

    // The ledger, per currency: the cash, the account's own exchange rate for the currency, the realized and
    // unrealized profit in that currency. THE ROWS TO COMPARE with the fx rate smoke's rates and with the
    // engine's `equity` view after the drill.
    let group = AccountGroup("All".to_string());
    let sub = client
        .account_summary(
            &group,
            &[AccountSummaryTags::LEDGER_ALL, AccountSummaryTags::NET_LIQUIDATION],
        )
        .expect("account summary");
    let end = Instant::now() + Duration::from_secs(15);
    let mut rows = Vec::new();
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_secs(5)) {
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(row)))) => rows.push(row),
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => break,
            Some(Ok(SubscriptionItem::Notice(_))) => {}
            Some(Err(e)) => panic!("account summary: {e}"),
            None => break,
        }
    }
    drop(sub);
    let mine: Vec<_> = rows.iter().filter(|r| r.account == cfg.account).collect();
    eprintln!("-- {} summary rows for the account ({} in all)", mine.len(), rows.len());
    for row in &mine {
        if row.tag == "NetLiquidation" {
            eprintln!("NetLiquidation = {} {}   <- the base currency", row.value, row.currency);
        } else if LEDGER_TAGS.contains(&row.tag.as_str()) {
            eprintln!("ledger {:<26} {:<5} = {}", row.tag, row.currency, row.value);
        }
    }
    let has = |tag: &str| mine.iter().any(|r| r.tag == tag);
    eprintln!(
        "ExchangeRate rows present: {}   RealizedPnL rows present: {}   (documentation of both was not \
         confirmed; this is the measurement)",
        has("ExchangeRate"),
        has("RealizedPnL")
    );

    // Today's executions of the account with IB's own commission report: the currency IB charged the commission
    // in, and IB's `realizedPNL` of the closing fill (its currency is what this run is for).
    let filter = ExecutionFilter { account_code: cfg.account.clone(), ..Default::default() };
    let sub = client.executions(filter).expect("executions");
    let end = Instant::now() + Duration::from_secs(15);
    let mut seen = 0;
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_secs(5)) {
            Some(Ok(SubscriptionItem::Data(Executions::ExecutionData(ed)))) => {
                seen += 1;
                eprintln!(
                    "execution {} {} {:?} {} @ {} contract currency {} (placed by client {})",
                    ed.execution.execution_id,
                    ed.contract.symbol.0,
                    ed.execution.side,
                    ed.execution.shares,
                    ed.execution.price,
                    ed.contract.currency.0,
                    ed.execution.client_id
                );
            }
            Some(Ok(SubscriptionItem::Data(Executions::CommissionReport(c)))) => {
                eprintln!(
                    "commission {}: {} {}   realizedPNL {:?}",
                    c.execution_id, c.commission, c.currency, c.realized_pnl
                );
            }
            Some(Ok(SubscriptionItem::Notice(_))) => {}
            Some(Err(e)) => panic!("executions: {e}"),
            None => break,
        }
    }
    eprintln!("{seen} execution(s) today visible to client id {id}");
    eprintln!("IBKR fx figures smoke done: read-only, account untouched");
}
