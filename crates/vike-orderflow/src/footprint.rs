//! Footprint: one bar's per-price-bucket buy/sell volume grid. The cells are
//! `profile::PriceBin`s; `bar_agg::OrderflowAgg` builds the bars and `VolumeProfile::from_footprints`
//! merges them.
use crate::profile::PriceBin;

#[derive(Clone, PartialEq, Debug)]
pub struct FootprintBar {
    pub bar_index: u64,
    pub cells: Vec<PriceBin>,
}
