//! IG's feed arm: the two-verb Lightstreamer `Feeds`, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_gatewayhub_client::credentialed_limits;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::QuoteFeed;
use super::price_tap::PriceTap;
use crate::venue_arming::arming::{
    data_only_arming, ig_arming, mounts_exec_live, other_live_accounts, with_labelled_mount_remedy,
    with_other_live_accounts,
};

/// IG's feed: the Lightstreamer `Feeds` on the venue-agnostic `CoreLaneSink` — exactly two verbs,
/// a bar subscription per distinct `(epic, interval)` and a quote subscription per distinct epic
/// across this venue's mounts. Any subscription failure refuses the mount, except a CAP refusal of one epic's
/// quote lane (that epic runs without quotes and the start goes on) and a TRANSIENT one, which is asked again
/// (`super::subscribe_quotes_surviving`). Held as a [`QuoteFeed`]
/// (alpaca's shape): a tap in front of the core's sink records the last price per epic, and the feed's
/// watcher subscribes the QUOTE lane of an epic the control door admits (decision 0174).
///
/// **The door's lanes are capped by the venue's row** (`vike_gatewayhub_client::credentialed_limits::IG`, which the
/// datahub's broker counts for all holders together), read here so a lane past it is refused for want of a price
/// lane, naming the cap, with no round trip. The idle reaper frees only the lane of an epic NO account admitted, and
/// an admitted epic's lane (it prices an engine's position) stays open until its account's engine stops or the node
/// restarts. A failed subscribe is remembered for 30 s ([`QuoteFeed::with_lane_cap`]).
pub(super) fn wire_ig_feed(
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    // The credentialed-data shape over a THIRD kind of transport: Lightstreamer TLCP text
    // frames. Everything lands on the venue-agnostic `CoreLaneSink` (through
    // `wrap` like every other arm), NOT a `MakerSink`: IG's candle lane emits REAL
    // `close_bar`s on the lossless lane (`CandleFold` closes on `CONS_END` or on a `UTM`
    // advance, whichever arrives first — the second signal exists so a lost frame cannot
    // silently drop a close), so the margin/drawdown watchdogs sweep on the venue's own
    // candle closes and nothing needs synthesizing.
    //
    // ⚠ EXACTLY TWO VERBS, and this is where the arm must not be padded out to look like
    // its neighbours: `subscribe_trades`/`subscribe_book`/`subscribe_depth` are all caps
    // refusals on this venue (`venue_caps::IG` — a dealer publishes no tape and no
    // ladder), so calling one here would turn a structural venue fact into a mount error.
    // The maker requotes on `on_quote_tick` alone; `ig_arming` discloses exactly that.
    //
    // ⚠ MOUNT-FAILURE SEMANTICS are alpaca's, not ctrader's: the client builds NO socket at the
    // gatewayhub's open and each `subscribe_*` only starts a thread that then logs in and dials
    // with the driver's stop-aware backoff, so a wrong password or an unreachable gateway
    // surfaces as a reconnect-looping feed in the log, never a mount error. The credential
    // half is the gatewayhub's refusal of the credentialed stream (the account's `API_KEY` +
    // `IDENTIFIER` + `PASSWORD`), which reaches this arm as the first `subscribe_*`'s error.
    //
    // ONE IG login and ONE Lightstreamer socket for every subscription of the feed
    // (`crates/bridges/ig/src/ls_link.rs`'s `Link`, at the gatewayhub), and IG publishes no rate
    // gate this workspace paces against (`vike_venues::venue_rate_limits`'s `IG` declares none).
    // That login re-authenticates after token expiry on its own.
    //
    // The tap in front of the core's sink only RECORDS the last price per epic (every verb is forwarded
    // unchanged), so the control door can tell whether an epic it admitted has a price yet.
    let (mut feeds, tap) = super::dial("ig", None, PriceTap::default, handle, wrap, make)?;
    // One BAR subscription per distinct (epic, interval) across this venue's mounts — each
    // is its own TLCP subscription, so unlike ctrader two intervals genuinely both fire — and
    // one QUOTE subscription per distinct epic. The HL arm's dedup shape; every interval
    // here is the bridge's to refuse (`ig_plan` no longer holds the chart-scale table): an interval IG
    // does not stream refuses the bar subscription and so the mount.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.token_id, &c.interval));
            feeds
                .subscribe_bars(&c.token_id, &c.interval)
                .map_err(|e| format!("ig subscribe_bars {}@{}: {e:?}", c.token_id, c.interval))?;
        }
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            super::subscribe_quotes_surviving("ig", &mut feeds, &c.token_id, make)?;
        }
    }
    Ok(QuoteFeed::new(feeds, tap).with_lane_cap(credentialed_limits::IG.max_streams))
}

/// IG's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_ig_arming(
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = mounts_exec_live("ig", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("ig") && !exec_live {
            // The DECLARED exception — same shape as the alpaca/oanda branches above.
            data_only_arming(ig_arming(false), "ig")
        } else {
            with_labelled_mount_remedy(ig_arming(exec_live), "ig", mount_accounts)
        },
        "ig",
        other_live_accounts("ig", venue_arming, live_venues, mount_accounts),
    );
    let message = match &arming.remedy {
        None => format!(
            "LIVE assemble_runtime core up; ig feed subscribed (Lightstreamer L1 quotes + \
             streamed candle closes, both over one IG login) — ⚠ EXEC IS \
             LIVE ON {network}: orders from this mount are REAL orders against the ig \
             {network} account whose credentials were found",
            network = arming.network
        ),
        Some(remedy) => format!(
            "LIVE assemble_runtime core up; ig feed subscribed (Lightstreamer L1 quotes + \
             streamed candle closes, both over one IG login) — but EXEC IS \
             PAPER for ig and NO ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
        ),
    };
    warn_arming_banner!(cfg.venue, cfg, cfgs, arming, live_venues, message);
}
