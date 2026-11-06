//! The live smokes' one way to the venue settings: the `ibkr` gateway rows from the settings
//! database, plus the socket smokes' shared Gateway reads (`connect`, `stock`, `aapl`, `observe`,
//! behind the `ibkr` feature every one of those members is gated on). Declared ONCE by the
//! `ibkr_smokes` group binary (`main.rs`), so it compiles once for all its smokes and probes.

#[cfg(feature = "ibkr")]
use std::time::{Duration, Instant};

#[cfg(feature = "ibkr")]
use ibapi::client::blocking::Client;
#[cfg(feature = "ibkr")]
use ibapi::contracts::{Contract, Currency, Exchange, SecurityType, Symbol};
#[cfg(feature = "ibkr")]
use ibapi::orders::Orders;
#[cfg(feature = "ibkr")]
use ibapi::subscriptions::SubscriptionItem;

/// The venue's gateway rows — `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` (decision 0095) — read from
/// the settings database in the same settings directory the credential read above resolves.
///
/// ⚠ LOUD, never a quiet default: a database that will not read would otherwise hand this smoke
/// the built-in gateway — the self-skip that prints `ok` having tested nothing.
pub fn ibkr_settings(
    settings_dir_override: Option<&str>,
) -> vike_secrets::venue_setting::VenueSettings {
    vike_paths::state_path::project_settings_dir_from(
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

/// Connect, retrying a few times: an id the Gateway has only just been told to release answers `[326]` for a moment.
#[cfg(feature = "ibkr")]
pub fn connect(addr: &str, id: i32) -> Result<Client, String> {
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

/// A stock contract on `exchange`, quoted in `currency`.
#[cfg(feature = "ibkr")]
pub fn stock(symbol: &str, exchange: &str, currency: &str) -> Contract {
    Contract {
        symbol: Symbol::from(symbol),
        security_type: SecurityType::Stock,
        exchange: Exchange::from(exchange),
        currency: Currency::from(currency),
        ..Contract::default()
    }
}

/// The AAPL stock on SMART, in dollars.
#[cfg(feature = "ibkr")]
pub fn aapl() -> Contract {
    stock("AAPL", "SMART", "USD")
}

/// What the observer client (its own id) sees: this run's open orders (by `orderRef` prefix) and the stock's position.
#[cfg(feature = "ibkr")]
pub fn observe(
    addr: &str,
    id: i32,
    account: &str,
    ticker: &str,
    prefix: &str,
) -> Result<(Vec<String>, f64), String> {
    let client = Client::connect(addr, id).map_err(|e| format!("observer connect: {e}"))?;
    let mut open = Vec::new();
    let sub = client.all_open_orders().map_err(|e| format!("open orders: {e}"))?;
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(Orders::OrderData(o)))) => {
                if o.order.order_ref.starts_with(prefix) {
                    open.push(format!(
                        "{} id={} status={}",
                        o.order.order_ref,
                        o.order_id,
                        o.order_state.status.as_str()
                    ));
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("open orders: {e}")),
            None => break,
        }
    }
    let mut position = 0.0;
    let sub = client.positions().map_err(|e| format!("positions: {e}"))?;
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        match sub.next_timeout(Duration::from_millis(500)) {
            Some(Ok(SubscriptionItem::Data(ibapi::accounts::PositionUpdate::Position(p)))) => {
                if p.account == account && p.contract.symbol == ticker {
                    position += p.position;
                }
            }
            Some(Ok(SubscriptionItem::Data(ibapi::accounts::PositionUpdate::PositionEnd))) => break,
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("positions: {e}")),
            None => break,
        }
    }
    client.disconnect();
    Ok((open, position))
}
