//! cTrader's feed arm: the daemon's own dedicated data socket, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_mount::MakerMountConfig;

use crate::venue_arming::arming::{
    ctrader_arming, data_only_arming, other_live_accounts, with_other_live_accounts,
};

/// cTrader's feed: the daemon's OWN dedicated data socket (connect + auth is SYNCHRONOUS, and a
/// failure refuses to start), with closed bars SYNTHESIZED from quote mids by a `MakerSink`, and
/// one quote subscription per distinct symbol across this venue's mounts.
pub(super) fn wire_ctrader_feed(
    config: &vike_ctrader::config::CtraderConfig,
    cfg: &MakerMountConfig,
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
) -> Result<vike_ctrader::data::CtraderData, String> {
    // The daemon's OWN data socket — `connect_and_auth` (data-only: no `EventSender`),
    // isolated from the exec socket cTrader's mount (`crates/bridges/ctrader/src/mount.rs`'s
    // `CtraderVenueMount`) opened inside `build_node`, the same one-connection-per-plane
    // rule every inline recon client
    // follows. The sink is a `MakerSink` (the polymarket shape), NOT a bare
    // `CoreLaneSink`: cTrader's live trendbar lane only ever emits FORMING snapshots
    // through this client (`conn::on_inbound` — no close fires), so real venue bars can
    // never drive the per-CLOSED-bar watchdogs; the `MakerSink` synthesizes closed bars
    // from quote mids at the mount interval instead, and every ctrader mount shares its
    // one window (`check_ctrader_intervals` refused disagreement before the core
    // spawned). Trendbars are deliberately NOT subscribed — the sink would no-op their
    // seed/forming verbs anyway, and mixing bid-priced venue history into a mid-priced
    // synth series helps nothing.
    //
    // ⚠ MOUNT-FAILURE SEMANTICS, and they deliberately DIVERGE from exec: this handshake
    // is SYNCHRONOUS, and a connect/auth failure FAILS `live_mount` — the daemon refuses
    // to start. The exec side demotes a failed ctrader connect to paper for the session
    // (a working paper mount is still a mount), but a live daemon whose FEED never
    // connected would hold a core that ticks nothing and watches nothing, the
    // silent-do-nothing this repo refuses to ship. Loud at startup beats dead at 3am.
    let sink = wrap(Arc::new(vike_mount::MakerSink::new(
        handle,
        cfg.venue.clone(),
        cfg.token_id.clone(),
        cfg.interval.clone(),
        cfg.interval_ms,
    )) as Arc<dyn LiveDataSink>);
    let conn =
        vike_ctrader::conn::connect_and_auth(config.to_conn_config(), sink).map_err(|e| {
            format!(
                "ctrader DATA connect/auth failed (the dedicated data socket; exec may \
                 separately have demoted to paper inside build_node): {e:?} — the daemon \
                 refuses to start rather than mount a feed-less live core; check \
                 demo.ctraderapi.com reachability and the DEMO token pair, then restart"
            )
        })?;
    let mut data = vike_ctrader::data::CtraderData::new(conn);
    // One QUOTE subscription per distinct symbol (every ctrader mount was pinned to the
    // one wired symbol by `ctrader_plan`, so this is one subscription today; the loop is
    // the same dedup shape as every other arm). The quote stream is the whole feed: it
    // drives `on_quote_tick` AND the bar synth above.
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            data.subscribe_quotes(&c.token_id)
                .map_err(|e| format!("ctrader subscribe_quotes {}: {e:?}", c.token_id))?;
        }
    }
    Ok(data)
}

/// cTrader's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_ctrader_arming(
    cfg: &MakerMountConfig,
    cfgs: &[&MakerMountConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = live_venues.contains("ctrader");
    let arming = with_other_live_accounts(
        if data_only.contains("ctrader") && !exec_live {
            // Paper-by-declaration — here it REPLACES the "restart to retry the handshake"
            // remedy, which would be doubly false: no handshake failed, and a restart would
            // change nothing (the withhold is deliberate and repeats every boot).
            data_only_arming(ctrader_arming(false), "ctrader")
        } else {
            ctrader_arming(exec_live)
        },
        "ctrader",
        other_live_accounts("ctrader", venue_arming, live_venues),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; ctrader feed subscribed (spot quotes over the \
             dedicated data socket; closed bars SYNTHESIZED from quote mids) — ⚠ EXEC IS \
             LIVE ON {network}: orders from this mount are REAL orders against the \
             ctrader {network} account whose tokens were found",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; ctrader feed subscribed (spot quotes over the \
             dedicated data socket; closed bars SYNTHESIZED from quote mids) — but EXEC \
             IS PAPER for ctrader and NO ORDER FROM THIS MOUNT WILL EVER REACH THE \
             VENUE: {remedy}",
        ),
    }
}
