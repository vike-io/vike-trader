//! `venue_read` — an OPERATOR's read of a Binance account, asked of the venue now: its OPEN ORDERS
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it through the adapter's
//! `VenueAdapter::venue_read`, and `trade venue open-orders binance` prints it). Nothing changes at the venue and
//! nothing is folded. The balance and the positions are not offered.
//!
//! ## Every book of the account, whichever engine asks
//! One key pair trades three books, each with a list of its own: spot, USDⓈ-M and COIN-M. The read asks all three
//! with NO `symbol` (the venue's pages: every symbol's open orders when it is omitted), so the spot engine's account
//! and its lane twin (`binance/PERP`) answer the same list, and a row's symbol names its book in the engine's spelling
//! (`BTCUSDT` spot, `BTCUSDT.P` USDⓈ-M, `BTCUSD_PERP.P` COIN-M). The USDⓈ-M book is two lists: `openOrders`, and the
//! Algo Service's `openAlgoOrders`, where a resting stop, stop-limit, take-profit or trailing stop lives
//! (`crate::reconcile`'s `ReconcileClientBinance::account_open_orders`). The rows go through the reconcile parsers,
//! so a row's kind, trigger and our order client id (the broker prefix stripped) read as they do there.
//!
//! ## What it does not list
//! A COIN-M conditional order (dapi has no Algo Service, and this bridge places none there), the legs of a spot order
//! LIST as one order (each leg is its own row), and anything outside the three books (options, portfolio margin).
//!
//! ## The hosts and the cost
//! The hosts are the account's `venue_url` rows at its tier (`crate::adapter`'s `hosts_of`, the start's lookup), and
//! the requests ride the reconcile client's transport on its short-timeout path. Without a symbol the venue charges
//! weight 80 for the spot list and 40 for each futures list (the pages' figures), so one read costs the USDⓈ-M IP
//! budget 80 of its 2400 a minute; the gatewayhub holds reads of one account apart and runs one per venue at a time.
//! ⚠ Any list that fails fails the read in words, so an account whose key may not read futures cannot list its spot
//! orders here either. Not measured: no deployed gatewayhub has made these calls without a symbol; the rows are the
//! reconcile reads' shapes (measured per symbol).

use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::RestTransport;
use vike_model::{VenueRead, VenueReadKind, not_offered_words};
use vike_secrets::credentials::Credentials;
use vike_utility::errors::CarriedRefusal;

use crate::VENUE;
use crate::execution::BinanceUrls;
use crate::reconcile::{Book, ReconcileClientBinance, reader};
use vike_bridge_core::signer::VenueClock;

/// The books the read asks, in order. Each client is built with no symbol (`crate::reconcile`'s `reader`): its book
/// alone picks its host and transport.
const BOOKS: [Book; 3] = [Book::Spot, Book::UsdM, Book::CoinM];

/// The answer to `kind` for the account whose pair is `creds`, on `hosts` (the refusal naming a missing address row,
/// said only when the open orders are asked). `link_id` is the broker link id the account's orders are placed under
/// (the start's `attribution_code_from`), which every row's client id is read with. `Err` is the operator's sentence.
pub(crate) fn venue_read(
    creds: &Credentials,
    hosts: Result<BinanceUrls, CarriedRefusal>,
    kind: VenueReadKind,
    link_id: Option<String>,
) -> Result<VenueRead, CarriedRefusal> {
    if kind != VenueReadKind::OpenOrders {
        return Err(not_offered_words(VENUE, kind).into());
    }
    let (hosts, clock) = (hosts?, VenueClock::default());
    read_books(
        &BOOKS.map(|book| {
            (book, reader(creds, book, "", hosts, &clock).with_link_id(link_id.clone()))
        }),
    )
    .map_err(Into::into)
}

/// The read itself, over each book's client: the seam the offline tests drive. The rows are cut at
/// [`vike_model::MAX_VENUE_OPEN_ORDERS`] with the cut said (the venue does not page these lists, so the cap is the
/// only cut).
fn read_books<S: Signer, T: RestTransport>(
    books: &[(Book, ReconcileClientBinance<S, T>)],
) -> Result<VenueRead, String> {
    let mut reports = Vec::new();
    for (book, client) in books {
        reports.extend(client.account_open_orders(*book)?);
    }
    // every row is read by the operator under its USER name (decision 0290)
    let (orders, truncated) =
        vike_model::open_orders_read(super::user_named(reports, |r| &mut r.symbol));
    Ok(VenueRead::OpenOrders { orders, truncated })
}

#[path = "venue_read_tests.rs"]
#[cfg(test)]
mod venue_read_tests;
