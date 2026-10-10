//! The unit tests of `tradehub_cli`, one child per concern; `NullSink`, `wired_1m_cfg` and
//! `assert_wired_and_advertised` are shared by several.

use super::*;
use crate::node::{account_admin_source, keys_for_account_capability};
use crate::summary::summary_line;
use crate::venue_arming::deadman::DeadManActionToCore;
use crate::venue_arming::deadman_absent_warning;
use vike_exec::CoreSnapshot;
use vike_tradehub_client::proto::Scope;

#[cfg(test)]
mod admin_barrier;
#[cfg(test)]
mod alpaca_ctrader;
#[cfg(test)]
mod args;
#[cfg(test)]
mod cex_arm;
#[cfg(test)]
mod deadman;
#[cfg(test)]
mod deribit;
#[cfg(test)]
mod exec_badge;
#[cfg(test)]
mod feed_splice;
#[cfg(test)]
mod ig;
#[cfg(test)]
mod oanda;
#[cfg(test)]
mod paths_and_alerts;
#[cfg(test)]
mod precedence;
#[cfg(test)]
mod ready_banner;
#[cfg(test)]
mod settings_and_reconcile;
#[cfg(test)]
mod stop_and_deadlines;
#[cfg(test)]
mod summary_line;

/// A `LiveDataSink` that discards everything — enough to construct a real venue `Feeds`, which
/// connects nothing until `subscribe_*` is called, so the tests below build genuine feed objects
/// without touching the network.
struct NullSink;

impl LiveDataSink for NullSink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_model::Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: vike_model::QuoteTick) {}
    fn trade(&self, _v: &str, _s: &str, _t: vike_model::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_model::L2Book>) {}
}

/// A 1m `MakerMountConfig` for `venue` on the symbol `build_node` actually mounts it on, at the
/// venue's own `tick_size` and a `qty` — the plan input every per-venue feed-arm child builds.
fn wired_1m_cfg(venue: &str, tick_size: f64, qty: f64) -> MakerMountConfig {
    let symbol = wired_symbol_for(venue).unwrap_or_else(|| panic!("build_node mounts {venue}"));
    let mut cfg = MakerMountConfig::crypto(venue, symbol, tick_size, qty);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

/// `slug` is joined to the exec plane the way every live-wired slug is: an engine row in
/// `crate::wired_markets::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
fn assert_wired_and_advertised(slug: &str) {
    assert!(
        wired_symbol_for(slug).is_some(),
        "{slug} has a live feed arm but `build_node` mounts no engine for it"
    );
    assert!(
        crate::config::LIVE_WIRED_VENUES.contains(&slug),
        "{slug} has a feed arm but is not advertised in LIVE_WIRED_VENUES"
    );
}
