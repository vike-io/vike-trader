//! cTrader's feed arm: the daemon's own dedicated data socket, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::Tracked;
use crate::venue_arming::arming::{
    ctrader_arming, data_only_arming, mounts_exec_live, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// cTrader's feed: the gatewayhub's dedicated data socket (its connect + auth is SYNCHRONOUS at the
/// open, and a failure refuses to start), with closed bars SYNTHESIZED from quote mids by a
/// `MakerSink`, and one quote subscription per distinct symbol across this venue's mounts.
pub(super) fn wire_ctrader_feed(
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<Tracked, String> {
    // The feed is a credentialed stream at the gatewayhub, over its OWN dedicated data socket
    // (`connect_and_auth`, data-only: no `EventSender`), isolated from the exec socket cTrader's mount
    // (`crates/bridges/ctrader/src/adapter.rs`'s `CtraderVenueAdapter`) opens, the same
    // one-connection-per-plane rule every recon client follows. The sink is a `MakerSink` (the polymarket
    // shape), NOT a bare `CoreLaneSink`: cTrader's live trendbar lane only ever emits FORMING snapshots
    // through this client (`conn::on_inbound` — no close fires), so real venue bars can
    // never drive the per-CLOSED-bar watchdogs; the `MakerSink` synthesizes closed bars
    // from quote mids at the mount interval instead, and every ctrader mount shares its
    // one window (`check_ctrader_intervals` refused disagreement before the core
    // spawned). Trendbars are deliberately NOT subscribed — the sink would no-op their
    // seed/forming verbs anyway, and mixing bid-priced venue history into a mid-priced
    // synth series helps nothing.
    //
    // ⚠ MOUNT-FAILURE SEMANTICS, and they deliberately DIVERGE from exec: the handshake is SYNCHRONOUS
    // at the gatewayhub's open, so a connect/auth failure is the open's refusal, reaches this arm as the
    // first `subscribe_quotes`'s error text, and FAILS `live_runtime` — the daemon refuses
    // to start. The exec side demotes a failed ctrader connect to paper for the session
    // (a working paper mount is still a mount), but a live daemon whose FEED never
    // connected would hold a core that ticks nothing and watches nothing, the
    // silent-do-nothing this repo refuses to ship. Loud at startup beats dead at 3am.
    //
    // The rotation home (the project state directory, where a refreshed token pair is written back) is
    // the gatewayhub's now: it is the one writer of the rotated pair.
    let sink = wrap(Arc::new(vike_tradehub_core::MakerSink::new(
        handle,
        cfg.venue.clone(),
        cfg.token_id.clone(),
        cfg.interval.clone(),
        cfg.interval_ms,
    )) as Arc<dyn LiveDataSink>);
    let mut data = Tracked::new(make.client("ctrader", None, sink)?);
    // One QUOTE subscription per distinct symbol (every ctrader mount was pinned to the
    // one wired symbol by `ctrader_plan`, so this is one subscription today; the loop is
    // the same dedup shape as every other arm). The quote stream is the whole feed: it
    // drives `on_quote_tick` AND the bar synth above.
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            data.subscribe_quotes(&c.token_id).map_err(|e| {
                format!(
                    "ctrader subscribe_quotes {}: {e:?} — the daemon refuses to start rather \
                     than mount a feed-less live core; check the DEMO token pair and the \
                     gatewayhub's reach of demo.ctraderapi.com, then restart",
                    c.token_id
                )
            })?;
        }
    }
    Ok(data)
}

/// cTrader's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_ctrader_arming(
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = mounts_exec_live("ctrader", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("ctrader") && !exec_live {
            // Paper-by-declaration — here it REPLACES the "restart to retry the handshake"
            // remedy, which would be doubly false: no handshake failed, and a restart would
            // change nothing (the withhold is deliberate and repeats every boot).
            data_only_arming(ctrader_arming(false), "ctrader")
        } else {
            with_labelled_mount_remedy(ctrader_arming(exec_live), "ctrader", mount_accounts)
        },
        "ctrader",
        other_live_accounts("ctrader", venue_arming, live_venues, mount_accounts),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; ctrader feed subscribed (spot quotes over the \
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
            "LIVE assemble_runtime core up; ctrader feed subscribed (spot quotes over the \
             dedicated data socket; closed bars SYNTHESIZED from quote mids) — but EXEC \
             IS PAPER for ctrader and NO ORDER FROM THIS MOUNT WILL EVER REACH THE \
             VENUE: {remedy}",
        ),
    }
}
