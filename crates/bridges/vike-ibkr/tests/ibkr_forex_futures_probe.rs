//! LIVE PAPER PROBE (fix wave 3, item A65, run by hand, never scheduled): what the paper gateway says about IDEALPRO forex and CME/NYMEX futures,
//! each test on a client id of its own (clock-derived, never one the daemon dials). Every finding is PRINTED as a `B99 ...` line; the decoded
//! `ibapi` structs of the order, the fills, the commission and the positions are printed in `Debug` form (`B99 RAW ...`), which is what the offline
//! tests rebuild their fixtures from.
//!
//! | test | what it settles | orders |
//! |---|---|---|
//! | `ibkr_forex_futures_details_and_what_if` | contract details (min tick, market rules, size rules, hours) of the IDEALPRO pairs and the index and oil futures; a delayed and a real-time quote of each class; a WHAT-IF order (margin and commission per contract and per pair) | none (a what-if never reaches the market) |
//! | `ibkr_forex_eurusd_round_trip_at_the_idealpro_minimum`, `ibkr_forex_usdjpy_round_trip_at_the_idealpro_minimum`, `ibkr_forex_eurusd_odd_lot_round_trip`, `ibkr_future_mes_round_trip` | one minimum-size market round trip each (EUR.USD 20000 and an odd lot of 1000, USD.JPY 25000, one MES contract): the order, the fills, the commission, the positions and the per-currency cash before and after | YES, paper, flattened at the end |
//!
//! The sizes and the instruments are constants of the tests, not environment variables (the settings registry gate refuses an undeclared
//! environment read, tests included); the placing tests are selected by NAME.
//!
//! ⚠ NEVER scheduled and in no workflow: a lane run by hand on the CI box (the gateway is on the box), a client id of its own, never the node's:
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_forex_futures_probe <test name> -- --ignored --nocapture --test-threads=1
//!
//! Gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on the `socket` backend, exists and the
//! gateway answers; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

mod common;
use common::ibkr_settings;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::accounts::types::AccountGroup;
use ibapi::accounts::{AccountSummaryResult, PositionUpdate};
use ibapi::client::blocking::{Client, Subscription};
use ibapi::contracts::{Contract, ContractDetails, Currency, Exchange, SecurityType, Symbol};
use ibapi::market_data::MarketDataType;
use ibapi::market_data::realtime::TickTypes;
use ibapi::orders::{Action, Order, OrderUpdate, PlaceOrder, TimeInForce as IbTif};
use ibapi::subscriptions::SubscriptionItem;
use vike_ibkr::IbkrConfig;
use vike_ibkr::config::load_ibkr_config_from;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::IbkrBackend;

fn paper_config() -> Option<IbkrConfig> {
    vike_log::test_init();
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let cfg =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))?;
    if cfg.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; this probe speaks the IB Gateway socket"
        );
        return None;
    }
    // HARD SAFETY: paper only. This assert stays a hard failure, never a skip.
    assert!(
        cfg.account.starts_with("DU"),
        "probe must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    Some(cfg)
}

/// A client id this gateway has not seen from us (the daemon's are small and configured).
fn fresh_id(offset: i32) -> i32 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    44_000 + (secs % 3_000) as i32 * 4 + offset
}

fn connect(addr: &str, id: i32) -> Result<Client, String> {
    let mut last = String::new();
    for _ in 0..5 {
        match Client::connect(addr, id) {
            Ok(client) => return Ok(client),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    Err(last)
}

fn forex(base: &str, quote: &str) -> Contract {
    Contract {
        symbol: Symbol::from(base),
        security_type: SecurityType::ForexPair,
        exchange: Exchange::from("IDEALPRO"),
        currency: Currency::from(quote),
        ..Contract::default()
    }
}

fn future(symbol: &str, exchange: &str) -> Contract {
    Contract {
        symbol: Symbol::from(symbol),
        security_type: SecurityType::Future,
        exchange: Exchange::from(exchange),
        currency: Currency::from("USD"),
        ..Contract::default()
    }
}

fn say_details(label: &str, d: &ContractDetails) {
    let c = &d.contract;
    eprintln!(
        "B99 DETAILS {label}: conid {} symbol {:?} local {:?} sec {:?} exch {:?} primary {:?} ccy {:?} class {:?} month {:?} expiry {:?} mult {:?}",
        c.contract_id,
        c.symbol,
        c.local_symbol,
        c.security_type,
        c.exchange,
        c.primary_exchange,
        c.currency,
        c.trading_class,
        c.last_trade_date_or_contract_month,
        d.real_expiration_date,
        c.multiplier
    );
    eprintln!(
        "B99 DETAILS {label}:   min_tick {} magnifier {} market_rule_ids {:?} valid_exchanges {:?} min_size {:?} size_increment {:?} suggested_size_increment {:?} ev_multiplier {} contract_month {:?} last_trade_time {:?} under_conid {} tz {:?}",
        d.min_tick,
        d.price_magnifier,
        d.market_rule_ids,
        d.valid_exchanges,
        d.min_size,
        d.size_increment,
        d.suggested_size_increment,
        d.ev_multiplier,
        d.contract_month,
        d.last_trade_time,
        d.under_contract_id,
        d.time_zone_id
    );
    eprintln!("B99 DETAILS {label}:   order_types {:?}", d.order_types);
    eprintln!(
        "B99 DETAILS {label}:   liquid_hours {:?}",
        d.liquid_hours.iter().take(3).collect::<Vec<_>>()
    );
    eprintln!(
        "B99 DETAILS {label}:   trading_hours {:?}",
        d.trading_hours.iter().take(3).collect::<Vec<_>>()
    );
}

fn say_market_rules(client: &Client, label: &str, details: &[ContractDetails]) {
    let mut seen: Vec<i32> = Vec::new();
    for d in details {
        for id in d.market_rule_ids.iter().flat_map(|s| s.split(',')) {
            if let Ok(id) = id.trim().parse::<i32>()
                && !seen.contains(&id)
            {
                seen.push(id);
            }
        }
    }
    for id in seen {
        match client.market_rule(id) {
            Ok(rule) => {
                let rows: Vec<String> = rule
                    .price_increments
                    .iter()
                    .map(|p| format!("{}:{}", p.low_edge, p.increment))
                    .collect();
                eprintln!("B99 MARKET RULE {label} {id}: {}", rows.join(" "));
            }
            Err(e) => eprintln!("B99 MARKET RULE {label} {id}: failed {e}"),
        }
    }
}

/// Stream a contract's ticks for `secs` seconds in the given market-data type and print every price tick, type tick and notice.
fn quote(client: &Client, label: &str, c: &Contract, kind: MarketDataType, secs: u64) -> usize {
    if let Err(e) = client.switch_market_data_type(kind) {
        eprintln!("B99 QUOTE {label} {kind:?}: switch failed {e}");
        return 0;
    }
    let sub = match client.market_data(c).subscribe() {
        Ok(sub) => sub,
        Err(e) => {
            eprintln!("B99 QUOTE {label} {kind:?}: subscribe failed {e}");
            return 0;
        }
    };
    let end = Instant::now() + Duration::from_secs(secs);
    let mut prices = 0;
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(TickTypes::Price(p)))) => {
                prices += 1;
                eprintln!("B99 QUOTE {label} {kind:?}: {:?} {}", p.tick_type, p.price);
            }
            Some(Ok(SubscriptionItem::Data(TickTypes::PriceSize(p)))) => {
                prices += 1;
                eprintln!(
                    "B99 QUOTE {label} {kind:?}: {:?} {} size {}",
                    p.price_tick_type, p.price, p.size
                );
            }
            Some(Ok(SubscriptionItem::Data(TickTypes::MarketDataType(t)))) => {
                eprintln!("B99 QUOTE {label} {kind:?}: market data type {t:?}");
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                eprintln!("B99 QUOTE {label} {kind:?}: notice [{}] {}", n.code, n.message);
            }
            Some(Err(e)) => {
                eprintln!("B99 QUOTE {label} {kind:?}: stream error {e}");
                break;
            }
            Some(Ok(_)) | None => {}
        }
    }
    prices
}

/// A WHAT-IF order: nothing reaches the market; the venue answers with the margin and the commission the order would carry.
fn what_if(
    client: &Client,
    next_id: &mut i32,
    label: &str,
    c: &Contract,
    action: Action,
    qty: f64,
    account: &str,
) {
    let order_id = *next_id;
    *next_id += 1;
    let order = Order {
        action,
        total_quantity: qty,
        order_type: "MKT".to_string(),
        account: account.to_string(),
        what_if: true,
        ..Order::default()
    };
    let sub = match client.place_order(order_id, c, &order) {
        Ok(sub) => sub,
        Err(e) => {
            eprintln!("B99 WHATIF {label} qty {qty}: submit failed {e}");
            return;
        }
    };
    let end = Instant::now() + Duration::from_secs(8);
    let mut answered = false;
    while Instant::now() < end && !answered {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(PlaceOrder::OpenOrder(o)))) => {
                answered = true;
                eprintln!("B99 WHATIF {label} qty {qty}: order_state {:?}", o.order_state);
            }
            Some(Ok(SubscriptionItem::Data(PlaceOrder::OrderStatus(s)))) => {
                eprintln!("B99 WHATIF {label} qty {qty}: status {s:?}");
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                answered = true;
                eprintln!("B99 WHATIF {label} qty {qty}: notice [{}] {}", n.code, n.message);
            }
            Some(Err(e)) => {
                eprintln!("B99 WHATIF {label} qty {qty}: stream error {e}");
                break;
            }
            Some(Ok(_)) | None => {}
        }
    }
    if !answered {
        eprintln!("B99 WHATIF {label} qty {qty}: no answer in 8 s");
    }
}

#[test]
#[ignore = "live paper: read-only (what-if orders never reach the market); needs a running Gateway logged into a DU... account (see the module doc)"]
fn ibkr_forex_futures_details_and_what_if() {
    let Some(cfg) = paper_config() else { return };
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let id = fresh_id(0);
    let client = match connect(&addr, id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: could not connect to {addr} as client id {id}: {e}");
            return;
        }
    };
    let acct = cfg.account.as_str();
    let mut next_id = client.next_valid_order_id().expect("IB's next valid order id");
    eprintln!("B99 details probe: gateway {addr}, client id {id}, account {acct}");
    let mut answered = 0;

    for pair in ["EUR.USD", "USD.JPY", "GBP.USD"] {
        let Some((base, quote_ccy)) = pair.split_once('.') else { continue };
        let c = forex(base, quote_ccy);
        match client.contract_details(&c) {
            Ok(rows) => {
                answered += rows.len();
                for d in &rows {
                    say_details(pair, d);
                }
                say_market_rules(&client, pair, &rows);
                if let Some(d) = rows.first() {
                    for qty in [1.0, 1_000.0, 20_000.0, 25_000.0, 100_000.0] {
                        what_if(&client, &mut next_id, pair, &d.contract, Action::Buy, qty, acct);
                    }
                }
            }
            Err(e) => eprintln!("B99 DETAILS {pair}: failed {e}"),
        }
    }

    // Real-time forex (the owner's entitlement) and the delayed type on the same pair.
    let eurusd = forex("EUR", "USD");
    let live = quote(&client, "EUR.USD", &eurusd, MarketDataType::Realtime, 6);
    let delayed = quote(&client, "EUR.USD", &eurusd, MarketDataType::Delayed, 6);
    eprintln!("B99 EUR.USD price ticks: real-time {live}, delayed {delayed}");

    for spec in ["ES:CME", "MES:CME", "NQ:CME", "MNQ:CME", "CL:NYMEX"] {
        let Some((sym, exch)) = spec.split_once(':') else { continue };
        let mut rows = match client.contract_details(&future(sym, exch)) {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("B99 DETAILS {sym}: failed on {exch}: {e}");
                continue;
            }
        };
        answered += rows.len();
        eprintln!("B99 DETAILS {sym}.{exch}: {} contract month(s) listed", rows.len());
        rows.sort_by(|a, b| {
            a.contract
                .last_trade_date_or_contract_month
                .cmp(&b.contract.last_trade_date_or_contract_month)
        });
        for d in rows.iter().take(3) {
            say_details(&format!("{sym}.{exch}"), d);
        }
        say_market_rules(&client, sym, &rows[..rows.len().min(1)]);
        if let Some(front) = rows.first() {
            // The nearest month is the one a quote is asked for and a what-if is sent on; its expiry is printed above.
            let delayed = quote(&client, sym, &front.contract, MarketDataType::Delayed, 8);
            let live = quote(&client, sym, &front.contract, MarketDataType::Realtime, 5);
            eprintln!("B99 {sym} price ticks: delayed {delayed}, real-time {live}");
            what_if(
                &client,
                &mut next_id,
                &format!("{sym} BUY"),
                &front.contract,
                Action::Buy,
                1.0,
                acct,
            );
            what_if(
                &client,
                &mut next_id,
                &format!("{sym} SELL"),
                &front.contract,
                Action::Sell,
                1.0,
                acct,
            );
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    client.disconnect();
    assert!(answered > 0, "no contract details came back: nothing was measured");
}

/// Everything the order-update stream said about ONE order.
#[derive(Default)]
struct Seen {
    statuses: Vec<String>,
    notices: Vec<(i32, String)>,
    /// (execution id, shares, price, exchange)
    fills: Vec<(String, f64, f64, String)>,
    /// (execution id, commission, currency, realized pnl)
    commissions: Vec<(String, f64, String, Option<f64>)>,
    /// The decoded bodies, `Debug` form: what the offline tests rebuild their fixtures from.
    raw: Vec<String>,
}

impl Seen {
    fn ended(&self) -> bool {
        self.statuses.iter().any(|s| {
            s.ends_with("Filled")
                || s.ends_with("Cancelled")
                || s.ends_with("ApiCancelled")
                || s.ends_with("Inactive")
        })
    }
    fn filled_qty(&self) -> f64 {
        self.fills.iter().map(|f| f.1).sum()
    }
    fn say(&self, what: &str) {
        let notices: Vec<String> = self.notices.iter().map(|(c, t)| format!("[{c}] {t}")).collect();
        eprintln!("B99   {what}");
        eprintln!("B99     statuses {:?}", self.statuses);
        eprintln!(
            "B99     notices  {}",
            if notices.is_empty() { "-".into() } else { notices.join(" | ") }
        );
        for f in &self.fills {
            eprintln!("B99     FILL exec {} {} @ {} on {}", f.0, f.1, f.2, f.3);
        }
        for c in &self.commissions {
            eprintln!("B99     COMMISSION exec {} {} {} realizedPNL {:?}", c.0, c.1, c.2, c.3);
        }
        for r in &self.raw {
            eprintln!("B99     RAW {r}");
        }
    }
}

/// Read the stream for order `order_id` until `window` is up, or `settle` after the order ended (so a commission report that follows its
/// execution is caught).
fn collect(
    stream: &Subscription<OrderUpdate>,
    order_id: i32,
    window: Duration,
    settle: Duration,
) -> Seen {
    let mut seen = Seen::default();
    let deadline = Instant::now() + window;
    let mut ended_at: Option<Instant> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || ended_at.is_some_and(|t| t.elapsed() > settle) {
            break;
        }
        match stream.next_timeout(left.min(Duration::from_millis(250))) {
            Some(Ok(SubscriptionItem::Data(OrderUpdate::OpenOrder(o))))
                if o.order_id == order_id =>
            {
                seen.statuses.push(format!("openOrder:{}", o.order_state.status.as_str()));
                seen.raw.push(format!(
                    "OpenOrder contract {:?} order_state {:?}",
                    o.contract, o.order_state
                ));
            }
            Some(Ok(SubscriptionItem::Data(OrderUpdate::OrderStatus(s))))
                if s.order_id == order_id =>
            {
                seen.statuses.push(format!("orderStatus:{}", s.status.as_str()));
                seen.raw.push(format!("OrderStatus {s:?}"));
            }
            Some(Ok(SubscriptionItem::Data(OrderUpdate::ExecutionData(e))))
                if e.execution.order_id == order_id =>
            {
                seen.fills.push((
                    e.execution.execution_id.clone(),
                    e.execution.shares,
                    e.execution.price,
                    e.execution.exchange.clone(),
                ));
                seen.raw.push(format!(
                    "ExecutionData contract {:?} execution {:?}",
                    e.contract, e.execution
                ));
            }
            Some(Ok(SubscriptionItem::Data(OrderUpdate::CommissionReport(c)))) => {
                seen.commissions.push((
                    c.execution_id.clone(),
                    c.commission,
                    c.currency.clone(),
                    c.realized_pnl,
                ));
                seen.raw.push(format!("CommissionReport {c:?}"));
            }
            Some(Ok(SubscriptionItem::Notice(n))) if n.request_id == Some(order_id) => {
                seen.notices.push((n.code, n.message));
            }
            Some(Err(e)) => {
                seen.notices.push((0, format!("stream error: {e}")));
                break;
            }
            Some(Ok(_)) | None => {}
        }
        if ended_at.is_none() && seen.ended() {
            ended_at = Some(Instant::now());
        }
    }
    // Keep only the commission reports of THIS order's executions.
    let ids: Vec<String> = seen.fills.iter().map(|f| f.0.clone()).collect();
    seen.commissions.retain(|c| ids.contains(&c.0));
    seen
}

/// Every position the paper account holds, account-wide, in `Debug` form (the probe must leave nothing behind).
fn print_positions(client: &Client, label: &str) -> Vec<(String, f64)> {
    let mut rows = Vec::new();
    let sub = match client.positions() {
        Ok(sub) => sub,
        Err(e) => {
            eprintln!("B99   {label}: positions request failed: {e}");
            return rows;
        }
    };
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(PositionUpdate::Position(p)))) => {
                eprintln!(
                    "B99   {label}: POSITION {:?} sec {:?} ccy {:?} exch {:?} local {:?} qty {} avg cost {}",
                    p.contract.symbol,
                    p.contract.security_type,
                    p.contract.currency,
                    p.contract.exchange,
                    p.contract.local_symbol,
                    p.position,
                    p.average_cost
                );
                eprintln!("B99   {label}: RAW Position {p:?}");
                rows.push((format!("{:?}", p.contract.symbol), p.position));
            }
            Some(Ok(SubscriptionItem::Data(PositionUpdate::PositionEnd))) => break,
            Some(Err(e)) => {
                eprintln!("B99   {label}: positions stream error: {e}");
                break;
            }
            Some(Ok(_)) | None => {}
        }
    }
    eprintln!(
        "B99   {label}: {} position row(s) (a zero quantity is a closed position)",
        rows.len()
    );
    rows
}

/// The account's per-currency cash and rate rows (`$LEDGER:ALL`, the rows the reconcile balance reads).
fn print_ledger(client: &Client, label: &str) {
    let sub = match client.account_summary(&AccountGroup("All".to_string()), &["$LEDGER:ALL"]) {
        Ok(sub) => sub,
        Err(e) => {
            eprintln!("B99   {label}: ledger request failed: {e}");
            return;
        }
    };
    let end = Instant::now() + Duration::from_secs(10);
    let mut rows = 0;
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(s)))) => {
                rows += 1;
                if [
                    "CashBalance",
                    "ExchangeRate",
                    "RealizedPnL",
                    "UnrealizedPnL",
                    "NetLiquidationByCurrency",
                    "TotalCashBalance",
                ]
                .contains(&s.tag.as_str())
                {
                    eprintln!(
                        "B99   {label}: LEDGER {} {} {} ({})",
                        s.account, s.tag, s.value, s.currency
                    );
                }
            }
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => break,
            Some(Err(e)) => {
                eprintln!("B99   {label}: ledger stream error: {e}");
                break;
            }
            Some(Ok(_)) | None => {}
        }
    }
    eprintln!("B99   {label}: {rows} ledger row(s)");
}

/// Place one order, wait for its answer, cancel it if it is still working, and print everything the venue said.
fn place(
    client: &Client,
    stream: &Subscription<OrderUpdate>,
    next_id: &mut i32,
    contract: &Contract,
    what: &str,
    ord: &Order,
    window: Duration,
) -> Seen {
    let order_id = *next_id;
    *next_id += 1;
    eprintln!("B99 placing (order id {order_id}): {what}");
    if let Err(e) = client.submit_order(order_id, contract, ord) {
        eprintln!("B99   submit failed: {e}");
        return Seen::default();
    }
    let seen = collect(stream, order_id, window, Duration::from_secs(3));
    if !seen.ended() {
        let _ = client.cancel_order(order_id, "");
        let after = collect(stream, order_id, Duration::from_secs(4), Duration::from_secs(1));
        seen.say(&format!("{what}: STILL WORKING after {} s, cancelled", window.as_secs()));
        after.say("  after the cancel");
        return seen;
    }
    seen.say(what);
    seen
}

fn market(action: Action, qty: f64, account: &str, coid: &str) -> Order {
    Order {
        action,
        total_quantity: qty,
        order_type: "MKT".to_string(),
        tif: IbTif::Day,
        order_ref: coid.to_string(),
        account: account.to_string(),
        ..Order::default()
    }
}

#[test]
#[ignore = "live paper: places real paper forex orders; needs a running Gateway logged into a DU... account (see the module doc)"]
fn ibkr_forex_eurusd_round_trip_at_the_idealpro_minimum() {
    forex_round_trip("EUR", "USD", 20_000.0, 1);
}

#[test]
#[ignore = "live paper: places real paper forex orders; needs a running Gateway logged into a DU... account (see the module doc)"]
fn ibkr_forex_usdjpy_round_trip_at_the_idealpro_minimum() {
    forex_round_trip("USD", "JPY", 25_000.0, 3);
}

#[test]
#[ignore = "live paper: places real paper forex orders; needs a running Gateway logged into a DU... account (see the module doc)"]
fn ibkr_forex_eurusd_odd_lot_round_trip() {
    forex_round_trip("EUR", "USD", 1_000.0, 5);
}

/// One BASE.QUOTE pair bought and sold back at `qty` units of the base, on a client id of its own (`id_offset`).
fn forex_round_trip(base: &str, quote_ccy: &str, qty: f64, id_offset: i32) {
    let Some(cfg) = paper_config() else { return };
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let id = fresh_id(id_offset);
    let client = match connect(&addr, id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: could not connect to {addr} as client id {id}: {e}");
            return;
        }
    };
    let acct = cfg.account.as_str();
    let stream = client.order_update_stream().expect("the order-update stream");
    let mut next_id = client.next_valid_order_id().expect("IB's next valid order id");
    let pair = format!("{base}.{quote_ccy}");
    let asked = forex(base, quote_ccy);
    let details = client.contract_details(&asked).expect("the pair's contract details");
    let d = details.first().expect("one contract for the pair");
    let contract = d.contract.clone();
    assert!(qty > 0.0 && qty <= 100_000.0, "refusing a size of {qty} on a probe");
    eprintln!(
        "B99 round trip: {pair} conid {} size {qty}, gateway {addr}, client id {id}, account {acct}",
        contract.contract_id
    );
    say_details(&pair, d);
    round_trip(&client, &stream, &mut next_id, &contract, &pair, qty, acct, "b99fx");
    std::thread::sleep(Duration::from_millis(500));
    client.disconnect();
}

/// BUY `qty`, print what the venue said and the account's positions and ledger, then SELL what was bought. Only two ledger reads: the paper gateway
/// refuses a third account-summary request of one client ([322]).
fn round_trip(
    client: &Client,
    stream: &Subscription<OrderUpdate>,
    next_id: &mut i32,
    contract: &Contract,
    label: &str,
    qty: f64,
    acct: &str,
    coid: &str,
) {
    print_positions(client, "before");
    print_ledger(client, "before");
    let buy = place(
        client,
        stream,
        next_id,
        contract,
        &format!("BUY {qty} {label} MKT"),
        &market(Action::Buy, qty, acct, &format!("{coid}buy")),
        Duration::from_secs(20),
    );
    let held = buy.filled_qty();
    print_positions(client, "after the buy");
    print_ledger(client, "after the buy");
    if held > 0.0 {
        let sell = place(
            client,
            stream,
            next_id,
            contract,
            &format!("SELL {held} {label} MKT (the flatten)"),
            &market(Action::Sell, held, acct, &format!("{coid}sell")),
            Duration::from_secs(20),
        );
        let left = held - sell.filled_qty();
        if left > 1e-9 {
            eprintln!("B99 !!! the flatten left {left} of {label} held: sell it by hand");
        }
    } else {
        eprintln!("B99 the buy did not fill: nothing to flatten");
    }
    print_positions(client, "after the flatten");
}

#[test]
#[ignore = "live paper: places real paper futures orders; needs a running Gateway logged into a DU... account (see the module doc)"]
fn ibkr_future_mes_round_trip() {
    let Some(cfg) = paper_config() else { return };
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let id = fresh_id(2);
    let client = match connect(&addr, id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: could not connect to {addr} as client id {id}: {e}");
            return;
        }
    };
    let acct = cfg.account.as_str();
    let stream = client.order_update_stream().expect("the order-update stream");
    let mut next_id = client.next_valid_order_id().expect("IB's next valid order id");
    // The micro E-mini S&P: one contract is about 38000 USD of notional, the smallest future the account can hold.
    let (root, exchange) = ("MES", "CME");
    let mut rows =
        client.contract_details(&future(root, exchange)).expect("the future's contract details");
    rows.sort_by(|a, b| {
        a.contract
            .last_trade_date_or_contract_month
            .cmp(&b.contract.last_trade_date_or_contract_month)
    });
    let d = rows.first().expect("one listed month");
    let contract = d.contract.clone();
    eprintln!(
        "B99 round trip: {root}.{exchange} conid {} local {:?} size 1, gateway {addr}, client id {id}, account {acct}",
        contract.contract_id, contract.local_symbol
    );
    say_details(root, d);
    round_trip(
        &client,
        &stream,
        &mut next_id,
        &contract,
        &contract.local_symbol,
        1.0,
        acct,
        "b99fut",
    );
    std::thread::sleep(Duration::from_millis(500));
    client.disconnect();
}
