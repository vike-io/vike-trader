//! The CEX feed arm: the kline lane, then the tick pump (adjacent, in that order), then the arming.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::DataClientSink;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MountPlacement;

use super::live::StrategyLaneCex;
use super::price_tap::{DataClientSinkPriceTap, PriceTap};
use super::pump_book::L2BookFold;
use super::{FeedRefusal, StrategyLaneFactories};
use crate::StrategyLanePlanCex;
use crate::venue_arming::arming::{
    cex_arming, mount_routes, mounts_execution_live, mounts_on_mainnet, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// The CEX kline feed (LANE 1): the venue's strategy-lane dial, one bar subscription per distinct
/// interval across this venue's mounts. Any subscription failure refuses the mount. With NO `configs` it is
/// the dial and the price tap alone (`wire_cex_price_feed`, a venue no mount names).
///
/// The ONE dial serves both lanes: its sink is the venue-agnostic `CoreLaneSink` behind a
/// [`L2BookFold`], which folds the tick pump's anchor-plus-delta book back into the whole-book
/// `DataClientSink::book` the core has always been handed (and passes every other verb, the bar verbs
/// included, straight through).
///
/// ⚠ Keep `spawn_cex_tick_pump` DIRECTLY below this function: a text gate reads from here to that
/// function's lane-2 banner and requires the bar verb in it and no other verb
/// (`tradehub_cli::tests`' `the_cex_arm_subscribes_only_the_bar_verb_in_the_kline_lane`).
pub(super) fn wire_cex_kline_lane(
    slug: &str,
    configs: &[&MountPlacement],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLaneCex, FeedRefusal> {
    // ── LANE 1: the KLINE feed, through the venue-agnostic `CoreLaneSink` (through `wrap`
    // exactly like the HL arm — an identity since the `record-feeds` tee it carried was
    // deleted on 2026-09-22). Bars
    // are what give the account-wide margin-call watchdog and the equity-drawdown latch a
    // cadence — both sweep per CLOSED bar and nowhere else — so this subscription is a
    // SAFETY dependency, not a display one.
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn DataClientSink>;
    // The tap sits between the fold and the core and only RECORDS the last price per symbol, so the
    // control door can ask whether an admitted symbol has one (decision 0174).
    let tap = Arc::new(PriceTap::default());
    let bar_sink = wrap(Arc::new(L2BookFold::new(Arc::new(DataClientSinkPriceTap::new(
        core,
        Arc::clone(&tap),
    )))) as Arc<dyn DataClientSink>);
    // The kline shell's own per-venue knobs (each CEX feed's `mark_streams` row, decision 0095; aster's
    // MAINNET pin, which the old arm spelled as `Feeds::with_env(.., AccountTier::Live)`) are the
    // gatewayhub's, applied when it builds the venue's client.
    let mut feed = StrategyLaneCex::new(make.strat(slug, None, bar_sink)?, tap);
    // One subscription per DISTINCT interval across this venue's mounts (their symbol is
    // the symbol the venue's default account states — `venue_feed_plan` refused anything else per mount): two
    // mounts on one series subscribe once, two intervals subscribe twice on the ONE feed.
    let mut intervals: Vec<&str> = Vec::new();
    for c in configs {
        if intervals.contains(&c.interval.as_str()) {
            continue;
        }
        intervals.push(&c.interval);
        // `BTCUSDT.P` here for aster: the kline lane's own `split_perp` maps it to the fapi perp stream
        // while the SERIES keeps the `.P` label `accepts_symbol` needs.
        feed.subscribe_bars(&c.symbol, &c.interval).map_err(|e| {
            FeedRefusal::subscribe(format!("{slug} subscribe_bars {}@{}", c.symbol, c.interval), &e)
        })?;
    }
    Ok(feed)
}

/// The CEX quote/trade/book pump (LANE 2): the venue's tick pump at the gatewayhub, over the same dial.
/// This is the lane the maker acts on and the daemon's link-death signal.
pub(super) fn spawn_cex_tick_pump(
    venue: &StrategyLanePlanCex,
    config: &MountPlacement,
    feed: &mut StrategyLaneCex,
) -> Result<(), FeedRefusal> {
    // ── LANE 2: the QUOTE/TRADE/BOOK pump. This is the
    // lane the maker acts on (`on_quote_tick`/`on_order_book` are the only two verbs
    // `SpreadMaker` reaches `requote` from). ⚠ Do NOT "simplify" this into the bar lane
    // above by calling `subscribe_depth`: that lane emits `DataClientSink::l2_snapshot`,
    // which is a DEFAULT NO-OP in every sink this daemon owns — the mount would look
    // perfectly healthy and post zero orders forever. See `StrategyLaneCex`'s doc.
    //
    // ⚠ **IT IS ALSO THIS DAEMON'S ONLY LINK-DEATH SIGNAL FOR A CEX VENUE.** The pump
    // discloses its own link state onto the same tick lane (one `Disconnected` per
    // outage, one `Live` on the first book back — it arrives as `StreamStatus` under the tube's
    // link word, and as the strat-link word when THIS process loses the datahub), and that is what arms
    // the CONNECTION-state dead-man here: `adapter_link_disclosure`'s `StrategyLanePlan::Cex` arm reads
    // exactly this subscription. Drop it, or move the maker to a lane that discloses
    // nothing, and four venues silently stop arming a switch that is ON by default —
    // `crates/vike-tradehub/src/tradehub_cli/tests/deadman.rs`'s
    // `the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link` is the gate that
    // refuses to let that happen quietly.
    //
    // `config.tick_size` sizes the book's price grid and was proven positive+finite by
    // `cex_plan`; it is the SAME number a maker mount quotes on (`MountPlacement::tick_size`), so the
    // pump's grid and the maker's quoting grid cannot disagree. The `.P` mount symbol goes in whole
    // for aster (the pump splits it to the exchange's `btcusdt` for the wire and labels every emitted
    // tick with the caller's own spelling, which is what `drive_strategy_tick` and `accepts_symbol`
    // dispatch on).
    feed.subscribe_ticks(&config.symbol, config.tick_size).map_err(|e| {
        let context =
            format!("{} subscribe_ticks {}@{}", venue.slug(), config.symbol, config.tick_size);
        FeedRefusal::subscribe(context, &e)
    })
}

/// The CEX arming disclosure: whether this venue's exec is LIVE or PAPER (and on which network),
/// in words, with the venue's other live accounts folded in. It is about the account(s) the venue's
/// mounts TRADE (`mount_accounts`; a mount naming none trades the default account), looked up by
/// route key in `live_venues` (decision 0172).
pub(super) fn announce_cex_arming(
    venue: &StrategyLanePlanCex,
    mainnet: &bool,
    slug: &str,
    config: &MountPlacement,
    configs: &[&MountPlacement],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::ArmingAccount],
    mount_accounts: &[AccountLabel],
) {
    // ⚠ SAY WHETHER THIS MOUNT'S EXEC IS LIVE OR PAPER, IN WORDS. An account that cannot run at its
    // tier is NOT RUNNING and its mount refuses the start (decision 0156), so what reaches this
    // line as "not live" is an account that really is `paper` (or one whose exec keys `data_only`
    // withheld on purpose). `live_venues` is `assemble_runtime`'s own record of which ACCOUNTS got a
    // live client, keyed by route key (the bare venue id for the default account, `venue#LABEL`
    // otherwise), so this reports what HAPPENED to the account the mount trades rather than what was
    // configured — and never asks the bare venue key on behalf of a labelled mount.
    let execution_live = mounts_execution_live(slug, mount_accounts, live_venues);
    let subject = mount_routes(slug, mount_accounts).join(", ");
    let mainnet = mounts_on_mainnet(slug, venue_arming, mount_accounts, *mainnet);
    // Everything this announcement CLAIMS is resolved by one pure function, so it is
    // testable without a socket and so the two arms below cannot drift apart. `network` and
    // `remedy` both exist because the previous pair got them wrong: it never said which
    // network a LIVE mount was about to trade on, and its PAPER advice named the DEMO key
    // set unconditionally — advice that arms nothing for an account whose tier is `live`,
    // which is precisely the state an operator is in while trying to go live. See
    // `ArmingCexAnnouncement::remedy`.
    let arming = with_other_live_accounts(
        with_labelled_mount_remedy(
            cex_arming(*venue, mainnet, execution_live),
            slug,
            mount_accounts,
        ),
        slug,
        other_live_accounts(slug, venue_arming, live_venues, mount_accounts),
    );
    let message = match &arming.remedy {
        None => format!(
            "LIVE assemble_runtime core up; {slug} feed subscribed (klines + quote/trade/book \
             pump) — ⚠ EXEC IS LIVE ON {network}: orders from this mount are REAL orders \
             against the {slug} {network} account whose credentials were found",
            network = arming.network
        ),
        Some(remedy) => format!(
            "LIVE assemble_runtime core up; {slug} feed subscribed (klines + quote/trade/book \
             pump) — but EXEC IS NOT LIVE for {subject}: the account this mount trades got no live \
             {slug} client in this process (its row is at tier `paper`, or its exec keys were \
             withheld on purpose by `data_only`), so NO ORDER FROM THIS MOUNT WILL EVER REACH \
             THE VENUE. An account that could not run at its tier would be NOT RUNNING and \
             would have refused this start, not landed here. To change it: {remedy}",
        ),
    };
    warn_arming_banner!(slug, config, configs, arming, live_venues, message);
}
