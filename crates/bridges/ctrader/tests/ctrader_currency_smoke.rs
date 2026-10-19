//! LIVE demo READ-ONLY smoke for the multi-currency drill (decision 0183, phase 1): what the account's deposit
//! currency IS, what the engine's rate source reads for a few quote currencies, and what the venue says its
//! last closing deals booked. It is the venue half of the demo drill that sets the engine's realized PnL
//! beside the venue's own figure (assumption 1 of the decision, UNMEASURED on any venue).
//!
//!     cargo test -p vike-ctrader --test ctrader_currency_smoke -- --ignored --nocapture
//!
//! READ-ONLY: places NO order, cancels nothing, changes nothing on the account. It opens a few short
//! sessions on its OWN sockets (the handshake for the deposit currency, one for the pairs' records, one quote
//! session per currency in [`CURRENCIES`], one deal list) and prints greppable lines, none of which carries a
//! credential or an account id:
//!
//! - `CTCCY deposit=<code>`: `ProtoOATrader.depositAssetId` named through the asset list;
//! - `CTSYM name=<pair> zone=... zone_read=<bool> trading_mode=... intervals=<n> first=... market=<Open|Closed|
//!   Unknown> enabled=... base=... quote=... lot=... min=... step=... commission=... commission_type=...
//!   min_commission_asset=... pnl_conversion_fee_rate=...`: the conversion pairs' own trading hours, the time
//!   zone name the broker sends (UNMEASURED: only a UTC zone is evaluated; `zone_read=false` holds the rate to
//!   the strict 120 s window, also over a weekend) and the market state the rate source would take, then the
//!   symbol's volume grid, commission configuration and conversion-fee rate;
//! - `CTRATE ccy=<code> account_per_quote=<x> inverse=<1/x> as_of_ms=<n>` or `CTRATE ccy=<code> error=<words>`:
//!   the engine's rate (`CtraderCatalog::quote_rate`: the mid of the conversion pair, ACCOUNT-currency units
//!   per ONE unit of `<code>`). An error naming "not a millisecond epoch" means the spot timestamp is in
//!   another unit than the assumed milliseconds; "no timestamp" means the venue sent none;
//! - `CTDEAL id=... sym=... gross=... swap=... comm=... balance=... q2d=... convfee=... digits=...
//!   detail_digits=... deal_digits=... trader_digits=...`: every CLOSING deal of the last [`WINDOW_HOURS`]
//!   hours (`CtraderReconClient::close_details`), `q2d` being `quoteToDepositConversionRate` read RAW and
//!   uninterpreted (the engine does not use it), and the three `moneyDigits` as the venue sent them;
//! - `CTPNL position=... sym=... side=... units=... gross=... net=... digits=...` and `CTPNLS count=<n>`: the
//!   venue's own unrealized PnL of every OPEN position (`CtraderReconClient::unrealized_pnls`), in the deposit
//!   currency.
//!
//! Credentials: the DEFAULT demo account's, resolved through `vike_secrets::smoke_directory` and the pure
//! `CtraderConfig::from_credentials` gate, with no rotation home: nothing here refreshes the grant. Absent
//! creds -> the test SKIPS.

use vike_ctrader::config::CtraderConfig;
use vike_ctrader::{CtraderCatalog, CtraderReconClient};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, now_ms};

/// The quote currencies the rate source is read for (the deposit currency is par, so it is skipped).
const CURRENCIES: [&str; 5] = ["JPY", "GBP", "EUR", "CHF", "USD"];

/// How far back the closing deals are listed.
const WINDOW_HOURS: i64 = 6;

/// The symbol the reconcile client is opened on (any listed symbol: the deal list is account-wide).
const SYMBOL: &str = "EURUSD";

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn ctrader_currency_readings_smoke() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = CtraderConfig::from_credentials(AccountTier::Demo, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
        return;
    };

    let catalog = CtraderCatalog::new(&credentials);
    let deposit = catalog.account_currency().expect("the account's deposit currency");
    println!("CTCCY deposit={deposit}");

    // What the pair records say about their own trading hours: the rate source reads the schedule only where
    // its time zone is UTC (`quote_rate::market_state`), so the zone the broker really sends is what decides
    // whether an open market's stale price is caught. `market=Unknown` here is the answer "not for this zone".
    let lists = vike_ctrader::conn::fetch_symbol_lists(config.to_conn_config())
        .expect("the account's symbol lists");
    let map = vike_ctrader::symbols::SymbolMap::from_lists(&lists);
    for name in ["USDJPY", "GBPUSD", "EURUSD", "USDCHF", "EURGBP"] {
        let Some(id) = map.id_of(name) else {
            println!("CTSYM name={name} record=none");
            continue;
        };
        let Some(record) = lists.full.iter().find(|s| s.symbol_id == id) else {
            println!("CTSYM name={name} record=none");
            continue;
        };
        let enabled = lists.light.iter().find(|l| l.symbol_id == id).and_then(|l| l.enabled);
        let market = vike_ctrader::quote_rate::market_state(record, now_ms());
        // `zone_read=false` is the answer "this zone is not evaluated": the rate source then holds the pair's
        // quote to the STRICT window (120 s), also over a weekend, instead of reading the schedule. Read it
        // BEFORE any order: a rate that is refused for its age is a drill that cannot start.
        println!(
            "CTSYM name={name} zone={:?} zone_read={} trading_mode={:?} intervals={} first={:?} holidays={} \
             market={market:?} enabled={enabled:?} base={:?} quote={:?} lot={:?} min={:?} step={:?} \
             commission={:?} commission_type={:?} min_commission={:?} min_commission_asset={:?} \
             pnl_conversion_fee_rate={:?}",
            record.schedule_time_zone,
            market != vike_ctrader::quote_rate::MarketState::Unknown,
            record.trading_mode,
            record.schedule.len(),
            record.schedule.first().map(|i| (i.start_second, i.end_second)),
            record.holiday.len(),
            map.base_asset(id),
            map.quote_asset(id),
            record.lot_size,
            record.min_volume,
            record.step_volume,
            // The base amounts are the PRECISE ones (the plain `commission` and `min_commission` are deprecated by
            // the vendor): the first scaled by 10^8, or 10^5 for a percentage-of-value type; the second by 10^8.
            record.precise_trading_commission_rate,
            record.commission_type,
            record.precise_min_commission,
            record.min_commission_asset,
            record.pnl_conversion_fee_rate,
        );
    }

    for ccy in CURRENCIES.into_iter().filter(|c| !c.eq_ignore_ascii_case(&deposit)) {
        match catalog.quote_rate(ccy) {
            Ok(rate) => println!(
                "CTRATE ccy={ccy} account_per_quote={} inverse={} as_of_ms={}",
                rate.account_per_quote.get(),
                1.0 / rate.account_per_quote.get(),
                rate.as_of_ms
            ),
            Err(why) => println!("CTRATE ccy={ccy} error={why}"),
        }
    }

    let client = CtraderReconClient::connect(&config.to_conn_config(), SYMBOL)
        .expect("a dedicated reconcile socket against the real cTrader demo endpoint");
    let since = now_ms() - WINDOW_HOURS * 3_600_000;
    let deals = client.close_details(since).expect("the account's deal list");
    for deal in &deals {
        println!("{}", deal.line());
    }
    println!("CTDEALS count={} window_hours={WINDOW_HOURS}", deals.len());

    // The venue's own unrealized PnL of every OPEN position (`ProtoOAGetPositionUnrealizedPnLReq`): empty on a
    // flat account. The drill holds a 10,000-unit position and reads this beside the engine's mark.
    match client.unrealized_pnls() {
        Ok(rows) => {
            for row in &rows {
                println!("{}", row.line());
            }
            println!("CTPNLS count={}", rows.len());
        }
        Err(why) => println!("CTPNLS error={why}"),
    }
}
