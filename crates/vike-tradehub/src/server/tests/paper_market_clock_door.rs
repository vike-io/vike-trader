//! A `paper` account follows its venue's market clock at the control door (the owner, 2026-11-21: "the same for other
//! venues as for IG"): while the venue's market is closed every order to it is refused in the FX week's own words, at
//! the send and at a Preview alike, and nothing reaches its book; on an open market the door is what it always was.
//! The venues are the ones whose session is the FX week (`vike_venues::session::session_for`), read from there.

use std::sync::Arc;

use vike_execution::{ArmingNotRunningView, MountView, OrderView, SymbolAdmission, VenueBlock};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::AccountTier;
use vike_tradehub_client::wire::CommandWire;

use super::admits_any_symbol::{Node, limit};
use crate::server::control::{ControlLimits, ControlLimitsConfig};
use crate::server::preview::{DoorReads, preview_verdict};
use crate::server::symbol_admission::{
    AccountReadiness, SymbolAdmitter, paper_market_closed_refusal,
};

/// Saturday 2026-11-14 12:00 UTC: the FX week is closed until Sunday 2026-11-15 21:00 UTC.
fn saturday_noon() -> i64 {
    vike_utility::calendar::days_from_civil(2026, 11, 14) * 86_400_000 + 12 * 3_600_000
}

/// Wednesday 2026-11-11 12:00 UTC: the FX week is open.
fn wednesday_noon() -> i64 {
    saturday_noon() - 3 * 86_400_000
}

/// The words every refusal starts with on [`saturday_noon`].
fn closed_words(venue: &str) -> String {
    format!(
        "FX market closed: login at 2026-11-15T21:00:00Z: a paper account of {venue} follows {venue}'s market clock"
    )
}

/// The roster venues on a market clock, from the code.
fn clocked_venues() -> Vec<&'static str> {
    vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| vike_venues::session::session_for(v) == vike_venues::session::FX_WEEK)
        .collect()
}

fn engine(venue: &str, symbol: &str, tier: AccountTier) -> VenueBlock {
    VenueBlock {
        venue: venue.into(),
        route_key: venue.into(),
        symbol: symbol.into(),
        tier: Some(tier),
        ..Default::default()
    }
}

/// **The rule, verb by verb**: on a closed Saturday an order to a paper account of every clocked venue is refused in
/// the closed words; a cancel is not judged; the same order on a Wednesday, to a demo account, or to a paper account of
/// a venue with no weekly close passes this gate.
#[test]
fn an_order_to_a_paper_account_of_a_clocked_venue_is_refused_while_its_market_is_closed() {
    let venues = clocked_venues();
    assert!(venues.contains(&"ig"), "{venues:?}");
    for venue in venues {
        let paper = [engine(venue, "EURUSD", AccountTier::Paper)];
        let order = limit("l-1", venue, "EURUSD", 1.0, 1.1);
        let why = paper_market_closed_refusal(&order, &paper, saturday_noon())
            .unwrap_or_else(|| panic!("{venue}: refused on a closed market"))
            .text;
        assert!(why.starts_with(&closed_words(venue)), "{why}");
        assert!(why.contains("The order was REFUSED"), "{why}");
        assert_eq!(paper_market_closed_refusal(&order, &paper, wednesday_noon()), None, "{venue}");
        let cancel = CommandWire::Cancel("l-1".into());
        assert_eq!(paper_market_closed_refusal(&cancel, &paper, saturday_noon()), None, "{venue}");
        let demo = [engine(venue, "EURUSD", AccountTier::Demo)];
        assert_eq!(paper_market_closed_refusal(&order, &demo, saturday_noon()), None, "{venue}");
    }
    let crypto = [engine("bybit", "BTCUSDT", AccountTier::Paper)];
    let order = limit("l-1", "bybit", "BTCUSDT", 1.0, 1.0);
    assert_eq!(paper_market_closed_refusal(&order, &crypto, saturday_noon()), None);
}

/// A source with a bar series of every symbol, holding nothing back (the paper account's own symbol passes the plan).
struct Priced;

impl SymbolAdmitter for Priced {
    fn admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        panic!("an order on the paper engine's own symbol looks nothing up at the door")
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(None)
    }
    fn bar_series(&self, _: &str, _: &str) -> bool {
        true
    }
}

impl AccountReadiness for Priced {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// The node's published engines, as a Preview reads them.
struct Reads(Vec<VenueBlock>);

impl DoorReads for Reads {
    fn engine_venues(&self) -> Vec<String> {
        self.0.iter().map(|b| b.venue.clone()).collect()
    }
    fn engine_route_keys(&self) -> Vec<String> {
        self.0.iter().map(|b| b.route_key.clone()).collect()
    }
    fn engine_blocks(&self) -> Vec<VenueBlock> {
        self.0.clone()
    }
    fn not_running(&self) -> Arc<Vec<ArmingNotRunningView>> {
        Arc::new(Vec::new())
    }
    fn orders_named_by(&self, _: &CommandWire) -> Vec<OrderView> {
        Vec::new()
    }
    fn mounts_named_by(&self, _: &CommandWire) -> Vec<MountView> {
        Vec::new()
    }
    fn symbol_admitter(&self) -> Option<&dyn SymbolAdmitter> {
        Some(&Priced)
    }
}

/// **End to end, the closed weekend and the open week**: through the ONE acceptance path into a real core, an order
/// to a paper oanda account on a closed Saturday is refused in the closed words and never reaches the book (the venue
/// client the engine sends to), and a Preview says the same; on the Wednesday the same order is sent, and the Preview
/// passes it.
#[test]
fn the_door_refuses_a_closed_markets_paper_order_and_sends_it_once_the_market_is_open() {
    let mut node = Node::start("oanda", "EURUSD", AccountTier::Paper);
    let mut preview =
        ControlLimits::new(ControlLimitsConfig { max_notional: None, rate_per_sec: 1e9 });
    let reads = Reads(node.blocks());

    node.limits.set_clock(saturday_noon);
    preview.set_clock(saturday_noon);
    let order = limit("closed-1", "oanda", "EURUSD", 1.0, 1.1);
    let why = node.send(order.clone(), Some(&Priced)).expect_err("refused while closed");
    assert!(why.starts_with(&closed_words("oanda")), "{why}");
    assert_eq!(preview_verdict(&order, &reads, &mut preview).refusal.map(|r| r.text), Some(why));
    assert!(node.submitted.lock().unwrap().is_empty(), "nothing reached the book");

    node.limits.set_clock(wednesday_noon);
    preview.set_clock(wednesday_noon);
    let order = limit("open-1", "oanda", "EURUSD", 1.0, 1.1);
    assert_eq!(preview_verdict(&order, &reads, &mut preview).refusal, None);
    node.send(order, Some(&Priced)).expect("sent on an open market");
    node.venue_saw("open-1");
    node.core.shutdown_and_join();
}
