//! The live smokes' one way to the venue settings: the `ibkr` gateway rows from the settings
//! database, the client ids a smoke may dial ([`SMOKE_CLIENT_ID_BASE`]: never the daemon's), plus
//! the socket smokes' shared Gateway reads (`connect`, `stock`, `aapl`, `observe`,
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

/// The venue's gateway rows — `venue.ibkr.<tier>.backend` (decision 0095) and the `venue_url` addresses
/// (decision 0265; the release's seed when the table is absent) — read from the settings database in the
/// same settings directory the credential read above resolves.
///
/// ⚠ LOUD, never a quiet default: a database that will not read would otherwise hand this smoke
/// the built-in gateway — the self-skip that prints `ok` having tested nothing.
///
/// It also installs the box's KEPT lists (`<project>/instruments`, decision 0290): a smoke names an instrument by its
/// user name (`AAPL`, `EURUSD`), which the bridge reads through the operator's ibkr row, as the gatewayhub does. A box
/// without the row refuses the order naming the verb that adds it (`docs/ops/operator-instruments.md`).
pub fn ibkr_settings(
    settings_dir_override: Option<&str>,
) -> vike_secrets::venue_setting::VenueSettings {
    let settings_dir = vike_paths::state_path::project_settings_dir_from(
        settings_dir_override,
        &std::env::current_dir().expect("a working directory"),
    );
    if let Some(dir) = vike_paths::state_path::instruments_dir_beside(settings_dir.as_deref()) {
        vike_catalog::use_kept_lists(dir);
    }
    settings_dir
        .map(|dir| {
            vike_secrets::venue_setting::load_venue_settings(&dir)
                .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
        })
        .and_then(|mut all| all.remove("ibkr"))
        .unwrap_or_default()
}

/// **Every client id a smoke or probe dials is this or above; the daemon dials none of them.** IB Gateway admits ONE
/// connection per client id, so a smoke under an id a running daemon holds is refused or knocks the daemon's session
/// off (the CI box, 2026-11-09: the socket reconcile smoke dialled the daemon's reconcile id 9 and the gateway refused it,
/// "failed to fill whole buffer"). The daemon dials only an account's `CLIENT_ID`, `DATA_CLIENT_ID` and
/// `RECON_CLIENT_ID` (unset: `vike_ibkr::config::DEFAULT_CLIENT_ID` and the next two; the CI box sets 7, 8, 9), so a smoke
/// never dials the ids of the account it loads: it overrides them with one below. An operator id written at or above
/// this base is the one way left to collide, and nothing refuses one.
pub const SMOKE_CLIENT_ID_BASE: i32 = 1_000;

// The fixed ids, one per smoke that dialled the loaded account's own (the daemon's) ids until 2026-11-09, past every
// clock-derived range of the other smokes (the highest, `ibkr_forex_futures_probe`'s, ends below `BASE + 55_000`).
/// `ibkr_smoke`'s exec client.
pub const PLACE_CANCEL_CLIENT_ID: i32 = SMOKE_CLIENT_ID_BASE + 60_000;
/// `ibkr_market_data_smoke`'s data client.
pub const MARKET_DATA_CLIENT_ID: i32 = SMOKE_CLIENT_ID_BASE + 60_001;
/// `ibkr_socket_reconcile_smoke`'s reconcile client.
pub const SOCKET_RECONCILE_CLIENT_ID: i32 = SMOKE_CLIENT_ID_BASE + 60_002;
/// `ibkr_fx_smoke`'s data client (`ibkr_fx_rate_smoke`).
pub const FX_RATE_CLIENT_ID: i32 = SMOKE_CLIENT_ID_BASE + 60_003;
/// `ibkr_fx_smoke`'s own API client (`ibkr_fx_figures_smoke`).
pub const FX_FIGURES_CLIENT_ID: i32 = SMOKE_CLIENT_ID_BASE + 60_004;

/// No smoke id is one the daemon dials by default, and no two fixed smoke ids are one: the daemon's are read from
/// `vike_ibkr::config`'s constants, the smokes' from the ones above.
#[test]
fn no_smoke_client_id_is_one_the_daemon_dials() {
    use vike_ibkr::config::{DATA_CLIENT_ID_OFFSET, DEFAULT_CLIENT_ID, RECONCILE_CLIENT_ID_OFFSET};
    let daemon = [
        DEFAULT_CLIENT_ID,
        DEFAULT_CLIENT_ID + DATA_CLIENT_ID_OFFSET,
        DEFAULT_CLIENT_ID + RECONCILE_CLIENT_ID_OFFSET,
    ];
    let fixed = [
        PLACE_CANCEL_CLIENT_ID,
        MARKET_DATA_CLIENT_ID,
        SOCKET_RECONCILE_CLIENT_ID,
        FX_RATE_CLIENT_ID,
        FX_FIGURES_CLIENT_ID,
    ];
    assert!(daemon.iter().all(|&id| id < SMOKE_CLIENT_ID_BASE), "{daemon:?}");
    for id in fixed {
        assert!(id >= SMOKE_CLIENT_ID_BASE && !daemon.contains(&id), "smoke id {id}");
        assert_eq!(fixed.iter().filter(|&&other| other == id).count(), 1, "smoke id {id} twice");
    }
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
