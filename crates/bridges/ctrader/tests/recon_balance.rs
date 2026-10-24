//! `CtraderReconClient::fetch_balance` over the wire, against the in-process fake venue of
//! `tests/common/foreign_book.rs`: the account's cash is read off a FRESH `ProtoOATrader` request on
//! every call (a snapshot from the handshake would go stale after the first fill and raise a false
//! `BalanceDrift`), descaled by the trader's own `moneyDigits`, and refused in words when the figure
//! cannot be trusted as the engine's ledger.
//!
//! This is the seed the engine's wallet was missing: the client used to inherit the trait default
//! (`Ok(None)`, "this venue reports no balance"), so the engine of a cTrader account stayed on its
//! configured seed cash forever and its buying-power gate judged every order against that seed.

mod common;

use std::time::Duration;

use vike_ctrader::CtraderReconClient;
use vike_ctrader::conn::ConnConfig;
use vike_exec::recon::ReconClient;

use common::{FakeCtrader, ForeignBook};

/// A client mounted on EURUSD over `book`; `revive` is the idle age that re-handshakes the socket.
fn client(book: ForeignBook, revive: Duration) -> (FakeCtrader, CtraderReconClient) {
    let server = FakeCtrader::start_foreign_book(book);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let client = CtraderReconClient::connect_with_idle_revive(&cfg, "EURUSD", revive)
        .expect("recon client connects");
    (server, client)
}

/// The venue states 1,000,067 at two money digits: the engine's cash is 10,000.67 USD.
#[test]
fn the_scripted_trader_balance_is_the_reconcile_balance() {
    let (_server, client) = client(
        ForeignBook { balance: 1_000_067, ..Default::default() },
        Duration::from_secs(86_400),
    );
    assert_eq!(client.fetch_balance(), Ok(Some(10_000.67)));
}

/// A pass reads the balance on a socket the idle clock has just revived, as it reads everything else.
#[test]
fn the_balance_read_survives_a_revive_on_every_call() {
    let (_server, client) =
        client(ForeignBook { balance: 250_000, ..Default::default() }, Duration::ZERO);
    assert_eq!(client.fetch_balance(), Ok(Some(2_500.0)));
    assert_eq!(client.fetch_balance(), Ok(Some(2_500.0)));
}

/// A balance in a currency the engine's ledger may not be in is an ERROR, never a number: the reconcile
/// balance carries no currency label, and a EUR figure adopted into a USD ledger is a wrong wallet.
#[test]
fn a_non_usd_deposit_is_refused_not_adopted() {
    let eur = ForeignBook { balance: 1_000_067, deposit_asset: Some(5), ..Default::default() };
    let (_server, client) = client(eur, Duration::from_secs(86_400));
    let refusal = client.fetch_balance().expect_err("a EUR balance is not adopted");
    assert!(refusal.contains("deposit currency is EUR"), "{refusal}");
}

/// A deposit asset the broker's list does not name is a currency nobody can state: refused, not guessed.
#[test]
fn an_unnamed_deposit_currency_is_refused_not_guessed() {
    let unnamed =
        ForeignBook { balance: 1_000_067, deposit_asset: Some(999), ..Default::default() };
    let (_server, client) = client(unnamed, Duration::from_secs(86_400));
    let refusal = client.fetch_balance().expect_err("an unnamed currency is not adopted");
    assert!(refusal.contains("deposit currency is unknown"), "{refusal}");
}
