//! **Is each price feed's strategy lane to the datahub up?** The dials this process holds, as non-owning probes, read
//! by the publisher for the frame's `CoreSnapshotWire::price_link_down`.
//!
//! Registered where a dial is made ([`super::StrategyLaneFactoriesProduction`]), because an arm wraps its feed at once and keeps no handle
//! a reader could ask. A probe holds nothing up, and a dial that wants no session is not a lost one
//! (`vike_market_stream::market::StreamLinkReading::Idle`): a price-only dial with nothing subscribed rests parked after the
//! datahub's idle close, and a partial feed a refused arm dropped is gone, so neither may read as a dead datahub.
//!
//! A READING: no order, halt or disclosure follows from it. A lost lane already reaches the core as a `Disconnected`
//! (decision 0164 verdict 2); this only says so to an operator.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use vike_market_stream::market::{StreamLinkProbe, StreamLinkReading};
use vike_tradehub_client::wire::PriceLinkDownWire;

/// See the module doc.
#[derive(Default)]
pub struct PriceLinks(Mutex<Vec<Dial>>);

struct Dial {
    venue: String,
    probe: StreamLinkProbe,
    /// When this registry first read the dial lost, until it reads anything else.
    lost_since: Option<Instant>,
}

impl PriceLinks {
    /// Watch one more dial of `venue`. A venue with two dials (a retried arm) is one row of [`Self::lost`].
    pub fn add(&self, venue: &str, probe: StreamLinkProbe) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(Dial {
            venue: venue.to_string(),
            probe,
            lost_since: None,
        });
    }

    /// How many dials hold a session with the datahub now: what makes "none is lost" a reading of the datahub at all, since
    /// a dead datahub leaves every idle dial idle.
    pub fn up(&self) -> u32 {
        let dials = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let up = dials.iter().filter(|dial| matches!(dial.probe.reading(), StreamLinkReading::Up));
        u32::try_from(up.count()).unwrap_or(u32::MAX)
    }

    /// The venues with a lost dial, in venue order, each counting how many venues want a lane at all.
    pub fn lost(&self) -> Vec<PriceLinkDownWire> {
        let now = Instant::now();
        let mut dials = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut venues: BTreeMap<&str, Option<(Instant, String)>> = BTreeMap::new();
        for dial in dials.iter_mut() {
            let lost = match dial.probe.reading() {
                StreamLinkReading::Idle => {
                    dial.lost_since = None;
                    continue;
                }
                StreamLinkReading::Up => {
                    dial.lost_since = None;
                    None
                }
                StreamLinkReading::Lost(why) => Some((*dial.lost_since.get_or_insert(now), why)),
            };
            let row = venues.entry(dial.venue.as_str()).or_default();
            if lost.is_some() {
                *row = lost;
            }
        }
        let of = u32::try_from(venues.len()).unwrap_or(u32::MAX);
        venues
            .into_iter()
            .filter_map(|(venue, lost)| {
                let (since, why) = lost?;
                Some(PriceLinkDownWire {
                    venue: venue.to_string(),
                    why,
                    down_ms: u64::try_from(now.saturating_duration_since(since).as_millis())
                        .unwrap_or(u64::MAX),
                    of,
                })
            })
            .collect()
    }
}
