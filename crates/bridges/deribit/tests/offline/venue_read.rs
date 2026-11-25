//! An operator's read of a Deribit account, over the local WebSocket stand-in in `crate::fake_deribit_websocket`:
//! which call each kind makes, that the call spans the account, and that it is a read. No venue, no
//! credentials, no `#[ignore]`.

use vike_deribit::reconcile::venue_read::read_over;
use vike_deribit::refusals::DeribitVenueReadRefusal;
use vike_model::{VenueRead, VenueReadKind};

use crate::fake_deribit_websocket::{FakeDeribit, Session, rest_against};

/// `kind` over the many-currency stand-in: the answer (every row named by its USER name, decision 0290), and the
/// stand-in to ask what it was sent.
fn read(kind: VenueReadKind) -> (VenueRead, FakeDeribit) {
    let fake = FakeDeribit::spawn(vec![Session::ForeignBook("USDC")]);
    let answer = read_over(&rest_against(&fake), kind).expect("the stand-in answers");
    (answer, fake)
}

/// The open orders are the account-wide call, naming no currency and no instrument, so an order on an
/// instrument the client was not built on is listed, with our label.
#[test]
fn the_open_orders_read_spans_the_account_and_is_one_read() {
    let (answer, fake) = read(VenueReadKind::OpenOrders);
    let VenueRead::OpenOrders { orders, truncated } = answer else { panic!("{answer:?}") };
    assert!(!truncated);
    assert_eq!(
        orders[0].symbol, "ETHUSDC.P",
        "not the instrument the client names (`ETH_USDC-PERPETUAL`)"
    );
    assert_eq!(orders[0].order_client_id.as_deref(), Some("vike-1"));
    assert_eq!(fake.methods(), ["private/get_open_orders"]);
    let params = &fake.params_of("private/get_open_orders")[0];
    assert_eq!(params["type"], "all", "a resting stop is an open order too");
    assert!(params.get("currency").is_none() && params.get("instrument_name").is_none());
}

/// The positions are `currency=any`: the stand-in answers the other currencies' rows only to that word,
/// so a read scoped like the reconcile client's would show one row.
#[test]
fn the_positions_read_spans_every_currency_and_says_long_or_short() {
    let (answer, fake) = read(VenueReadKind::Positions);
    let VenueRead::Positions { positions, truncated } = answer else { panic!("{answer:?}") };
    assert!(!truncated);
    let rows: Vec<_> =
        positions.iter().map(|p| (p.symbol.as_str(), p.outcome.as_str(), p.qty)).collect();
    assert_eq!(
        rows,
        [("BTCUSD.P", "long", 10.0), ("ETHUSD.P", "short", -5.0), ("ETHUSDC.P", "long", 0.5)]
    );
    assert_eq!(fake.methods(), ["private/get_positions"]);
    assert_eq!(fake.params_of("private/get_positions")[0]["currency"], "any");
}

/// The balance is the per-currency call, and every wallet with money crosses.
#[test]
fn the_balance_read_keeps_every_wallet_with_money() {
    let (answer, fake) = read(VenueReadKind::Balance);
    let VenueRead::Balance(balance) = answer else { panic!("{answer:?}") };
    assert_eq!((balance.asset.as_str(), balance.balance), ("BTC", 1.5));
    let others: Vec<_> =
        balance.other_assets.iter().map(|o| (o.asset.as_str(), o.balance)).collect();
    assert_eq!(others, [("USDC", 250.0)]);
    assert_eq!(fake.methods(), ["private/get_account_summaries"]);
}

/// A venue that answers with a refusal is the operator's sentence with the venue's own code, never an empty
/// answer ("nothing rests" would be a claim).
#[test]
fn a_venue_refusal_is_an_error_in_words() {
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorForever]);
    let rest = rest_against(&fake);
    for kind in [VenueReadKind::Balance, VenueReadKind::OpenOrders, VenueReadKind::Positions] {
        let why = read_over(&rest, kind).unwrap_err();
        assert!(matches!(why, DeribitVenueReadRefusal::Unread { code: 10009, .. }), "{why}");
    }
}
