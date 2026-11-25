//! `venue_read` — an OPERATOR's read of an OKX account, asked of the venue now: its OPEN ORDERS
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it through the adapter's
//! `VenueAdapter::venue_read`, and `trade venue open-orders okx` prints it). Nothing changes at the venue and nothing
//! is folded. The balance and the positions are not offered.
//!
//! ## Every instrument of the account, its stops included
//! `GET /api/v5/trade/orders-pending` with no `instId` and no `instType` lists every class's resting orders (a swap,
//! a future, a spot pair, the child order a fired stop placed), and `GET /api/v5/trade/orders-algo-pending` with no
//! `instId` lists the account's resting algo orders of one `ordType` (required): `conditional` (the stops, stop-limits
//! and take-profits this bridge places) and `move_order_stop` (its trailing orders), each walked to its last page. The
//! three reads and their parsers are the reconcile client's (`crate::reconcile`'s
//! `ReconcileClientOkx::account_open_orders`), so the two reads cannot disagree about a row: our order client id is the
//! `algoClOrdId` (a stop's, and a fired stop's child's) or the `clOrdId`, a stop's level is its TRIGGER, and a size
//! is the venue's own (contracts on a swap or a future, the coin on a spot pair: no instrument's `ctVal` is read).
//!
//! ## What it does not list, and the cut
//! The algo kinds this bridge does not place: `oco`, `trigger`, `iceberg`, `twap` and `chase` orders (their rows are no
//! kind the reconcile parser reads). The plain list is ONE page of 100 rows (the venue's maximum): a full page is
//! answered `truncated`, never walked. Any read failing fails the read in words.
//!
//! ## The host and the transport
//! The host is the account's REST base at its type and the `x-simulated-trading` header its tier selects
//! (`crate::execution::rest::OkxNet::resolve`, the lookup the start takes), on the short-timeout path: one read an operator asks
//! for stalls a gatewayhub thread five seconds a request at most. Not measured: no deployed gatewayhub has made these
//! calls without an `instId` (the reconcile reads ask per instrument; its account-wide trailing walk is unmeasured
//! too); the rows are the reconcile reads' shapes.

use crate::signer::OkxV5Signer;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::{VenueRead, VenueReadKind, not_offered_words};
use vike_secrets::credentials::Credentials;
use vike_utility::errors::CarriedRefusal;

use crate::execution::rest::{OkxNet, VENUE, simulated};
use crate::reconcile::ReconcileClientOkx;
use crate::transport::client::rest_transport_on;

/// The answer to `kind` for the account whose trio is `creds`, at the addresses `net` (the refusal naming a missing
/// address row, said only when the open orders are asked). `Err` is the operator's sentence.
pub(crate) fn venue_read(
    creds: &Credentials,
    net: Result<OkxNet, String>,
    kind: VenueReadKind,
) -> Result<VenueRead, CarriedRefusal> {
    if kind != VenueReadKind::OpenOrders {
        return Err(not_offered_words(VENUE, kind).into());
    }
    let net = net?;
    // A one-shot read: it reads the venue's clock for itself (owner ruling B-4).
    let clock = VenueClock::default();
    read_open_orders(&ReconcileClientOkx::new(
        OkxV5Signer::new(creds, clock.ms()),
        rest_transport_on(simulated(net.mainnet), &clock),
        net.rest,
        "",
        1.0,
    ))
    .map_err(Into::into)
}

/// The read itself, over any transport: the seam the offline tests drive. Cut at
/// [`vike_model::MAX_VENUE_OPEN_ORDERS`]; either cut is said.
fn read_open_orders<T: RestTransportJson>(
    client: &ReconcileClientOkx<T>,
) -> Result<VenueRead, String> {
    let (reports, more) = client.account_open_orders()?;
    let (orders, cut) = vike_model::open_orders_read(super::user_named(reports, |r| &mut r.symbol));
    // `more`: the venue's own page was full, which the shared cap cannot see.
    Ok(VenueRead::OpenOrders { orders, truncated: more || cut })
}

#[path = "venue_read_tests.rs"]
#[cfg(test)]
mod venue_read_tests;
