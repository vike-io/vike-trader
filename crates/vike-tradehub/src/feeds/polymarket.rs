//! Polymarket's feed arm (`polymarket` feature): one book-driven `Feeds` + `MakerSink` per token.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_mount::MakerMountConfig;

/// Polymarket's feeds: one `Feeds` + `MakerSink` per DISTINCT token (a sink folds every price it
/// sees into its token's synth bars), each driven by a single book subscription. Any subscription
/// failure refuses the mount.
#[cfg(feature = "polymarket")]
pub(super) fn wire_polymarket_feeds(
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
) -> Result<Vec<vike_polymarket::Feeds>, String> {
    // Polymarket's feed serves TICKS, not bars (`subscribe_bars` is Unsupported), so wire it
    // through `MakerSink` (NOT the bare CoreLaneSink): its inner CoreLaneSink forwards the
    // book-derived L1 quote onto the tick/market lanes (drives the maker's on_quote_tick /
    // on_order_book), AND its `TickBarSynthesizer` emits synth bars onto the bar lane (drives
    // the account-wide margin/drawdown sweeps the HL path gets from real bars). One
    // `subscribe_book` drives all of it. Keyless-public, but egresses through the SAME Dublin
    // SOCKS tunnel the exec side uses (US-geo-blocked otherwise).
    // One `Feeds` + `MakerSink` per DISTINCT token (split-plane I10): a `MakerSink` folds
    // EVERY price it sees into ITS token's synth bars, so two tokens through one sink
    // would corrupt both mounts' paper-fill bar lanes — see `LiveFeeds::Polymarket`. Two
    // mounts on ONE token share one entry (their interval agreement was refused-or-proven
    // by `check_poly_token_intervals` before the core spawned).
    let mut list: Vec<vike_polymarket::Feeds> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for c in cfgs {
        if seen.contains(&c.token_id.as_str()) {
            continue;
        }
        seen.push(&c.token_id);
        let sink = wrap(Arc::new(vike_mount::MakerSink::new(
            handle,
            c.venue.clone(),
            c.token_id.clone(),
            c.interval.clone(),
            c.interval_ms,
        )) as Arc<dyn LiveDataSink>);
        let mut feeds = vike_polymarket::Feeds::new(sink, || {});
        feeds
            .subscribe_book(&c.token_id)
            .map_err(|e| format!("subscribe_book {}: {e:?}", c.token_id))?;
        tracing::info!(
            venue = %c.venue, symbol = %c.token_id, interval = %c.interval,
            mounts = cfgs.len(), tokens = seen.len(),
            live_venues = ?live_venues,
            "LIVE build_node core up; Polymarket feed subscribed (book → derived L1 quote) — real \
             orders require flags.poly_exec + POLY_PRIVATE_KEY + Dublin egress"
        );
        list.push(feeds);
    }
    Ok(list)
}
