//! The CEX feed arm: the kline lane, then the tick pump (adjacent, in that order), then the arming.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_mount::MakerMountConfig;

use super::live::{CexBars, CexTicks};
use super::mark_streams_setting;
use crate::CexVenue;
use crate::venue_arming::arming::{cex_arming, other_live_accounts, with_other_live_accounts};

/// The CEX kline feed (LANE 1): the venue's own `Feeds`, one bar subscription per distinct interval
/// across this venue's mounts. Any subscription failure refuses the mount.
///
/// ⚠ Keep `spawn_cex_tick_pump` DIRECTLY below this function: a text gate reads from here to that
/// function's lane-2 banner and requires the bar verb in it and no other verb
/// (`tradehub_cli::tests`' `the_cex_arm_subscribes_only_the_bar_verb_on_the_status_bearing_handle`).
pub(super) fn wire_cex_kline_lane(
    venue: &CexVenue,
    slug: &str,
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    venue_settings: &std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> Result<CexBars, String> {
    // ── LANE 1: the KLINE feed, through the venue-agnostic `CoreLaneSink` (through `wrap`
    // exactly like the HL arm — an identity since the `record-feeds` tee it carried was
    // deleted on 2026-09-22). Bars
    // are what give the account-wide margin-call watchdog and the equity-drawdown latch a
    // cadence — both sweep per CLOSED BAR and nowhere else — so this subscription is a
    // SAFETY dependency, not a display one.
    let bar_sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>);
    // Each CEX feed takes its venue's `mark_streams` row (decision 0095); no row keeps the
    // feed's charter default.
    let mut bars = match venue {
        CexVenue::Binance => CexBars::Binance(
            vike_binance::market_feed::Feeds::new(bar_sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "binance")),
        ),
        CexVenue::Bybit => CexBars::Bybit(
            vike_bybit::market_feed::Feeds::new(bar_sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "bybit")),
        ),
        CexVenue::Okx => CexBars::Okx(
            vike_okx::market_feed::Feeds::new(bar_sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "okx")),
        ),
        // `with_env(.., Live)` and NOT `new`: aster's kline shell is `Environment`-keyed
        // (its one real delta from the binance template) and `Feeds::new` defaults to
        // TESTNET hosts — while this daemon's aster feed deliberately runs MAINNET, the
        // same choice vike-app's aster mount made, because exec resolves MAINNET-FIRST
        // from credentials and a paper/demo order must rest against the real book. See
        // `venue_feed_plan`'s aster arm for the whole argument.
        CexVenue::Aster => CexBars::Aster(
            vike_aster::market_feed::Feeds::with_env(
                bar_sink,
                || {},
                vike_bridge_core::credentials::Environment::Live,
            )
            .with_mark_streams(mark_streams_setting(venue_settings, "aster")),
        ),
    };
    // One subscription per DISTINCT interval across this venue's mounts (their symbol is
    // the venue's one wired symbol — `cex_plan` refused anything else per mount): two
    // mounts on one series subscribe once, two intervals subscribe twice on the ONE feed.
    let mut intervals: Vec<&str> = Vec::new();
    for c in cfgs {
        if intervals.contains(&c.interval.as_str()) {
            continue;
        }
        intervals.push(&c.interval);
        match &mut bars {
            CexBars::Binance(f) => f.subscribe_bars(&c.token_id, &c.interval),
            CexBars::Bybit(f) => f.subscribe_bars(&c.token_id, &c.interval),
            CexBars::Okx(f) => f.subscribe_bars(&c.token_id, &c.interval),
            // `BTCUSDT.P` here: the kline lane's own `split_perp` maps it to the fapi
            // perp stream while the SERIES keeps the `.P` label `accepts_symbol` needs.
            CexBars::Aster(f) => f.subscribe_bars(&c.token_id, &c.interval),
        }
        .map_err(|e| format!("{slug} subscribe_bars {}@{}: {e:?}", c.token_id, c.interval))?;
    }
    Ok(bars)
}

/// The CEX quote/trade/book pump (LANE 2): the venue's own `spawn_*_market_data`, straight onto
/// the core tick lane. This is the lane the maker acts on and the daemon's link-death signal.
pub(super) fn spawn_cex_tick_pump(
    venue: &CexVenue,
    cfg: &MakerMountConfig,
    handle: &CoreHandle,
) -> CexTicks {
    // ── LANE 2: the QUOTE/TRADE/BOOK pump, straight onto the core tick lane. This is the
    // lane the maker acts on (`on_quote_tick`/`on_order_book` are the only two verbs
    // `SpreadMaker` reaches `requote` from). ⚠ Do NOT "simplify" this into the `Feeds`
    // object above by calling `subscribe_depth`: that lane emits `LiveDataSink::l2_snapshot`,
    // which is a DEFAULT NO-OP in every sink this daemon owns — the mount would look
    // perfectly healthy and post zero orders forever. See `CexTicks`' doc.
    //
    // ⚠ **IT IS ALSO THIS DAEMON'S ONLY LINK-DEATH SIGNAL FOR A CEX VENUE.** The pump
    // discloses its own transport state onto the same tick lane (one `Disconnected` per
    // outage, one `Live` on the first frame back — `crates/bridges/binance/src/family/
    // depth.rs`'s `disclose_link` and its bybit/okx twins), and that is what arms the
    // CONNECTION-state dead-man here: `mount_link_disclosure`'s `VenuePlan::Cex` arm reads
    // exactly this subscription. Drop it, or move the maker to a lane that discloses
    // nothing, and four venues silently stop arming a switch that is ON by default —
    // `crates/vike-tradehub/src/tradehub_cli/tests/deadman.rs`'s
    // `the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link` is the gate that
    // refuses to let that happen quietly.
    //
    // `cfg.tick_size` sizes the book's price grid and was proven positive+finite by
    // `cex_plan`; it is the SAME number `MakerMountConfig::crypto` gave the maker, so the
    // pump's grid and the maker's quoting grid cannot disagree.
    let ticks = handle.tick_sender();
    match venue {
        CexVenue::Binance => {
            CexTicks::Binance(vike_binance::market_data::spawn_binance_market_data(
                ticks,
                &cfg.token_id,
                cfg.tick_size,
            ))
        }
        CexVenue::Bybit => CexTicks::Bybit(vike_bybit::market_data::spawn_bybit_market_data(
            ticks,
            &cfg.token_id,
            cfg.tick_size,
        )),
        CexVenue::Okx => CexTicks::Okx(vike_okx::market_data::spawn_okx_market_data(
            ticks,
            &cfg.token_id,
            cfg.tick_size,
        )),
        // `Environment::Live` pins the pump to the MAINNET futures hosts (same choice and
        // reason as the kline lane above). The `.P` mount symbol goes in whole: the pump
        // splits it to the exchange's `btcusdt` for the stream/snapshot wire and labels
        // every emitted tick with the caller's own spelling, which is what
        // `drive_strategy_tick` and `accepts_symbol` dispatch on — see
        // `vike_aster::market_data`'s `pump_wire`.
        CexVenue::Aster => CexTicks::Aster(vike_aster::market_data::spawn_aster_market_data(
            vike_bridge_core::credentials::Environment::Live,
            ticks,
            &cfg.token_id,
            cfg.tick_size,
        )),
    }
}

/// The CEX arming disclosure: whether this venue's exec is LIVE or PAPER (and on which network),
/// in words, with the venue's other live accounts folded in.
pub(super) fn announce_cex_arming(
    venue: &CexVenue,
    mainnet: &bool,
    slug: &str,
    cfg: &MakerMountConfig,
    cfgs: &[&MakerMountConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
) {
    // ⚠ SAY WHETHER THIS VENUE'S EXEC IS LIVE OR PAPER, IN WORDS. `build_node` mounts the
    // real `ExecutionClient` only when that venue's credentials are present, and the
    // fallback to paper is otherwise indistinguishable from a working live mount: same
    // startup, same feed, same quotes, and orders that go nowhere real. `live_venues` is
    // `build_node`'s own record of which venues got a live client, so this reports what
    // HAPPENED rather than what was configured.
    let exec_live = live_venues.contains(slug);
    // Everything this announcement CLAIMS is resolved by one pure function, so it is
    // testable without a socket and so the two arms below cannot drift apart. `network` and
    // `remedy` both exist because the previous pair got them wrong: it never said which
    // network a LIVE mount was about to trade on, and its PAPER advice named the DEMO key
    // tier unconditionally — advice that arms nothing whenever `{VENUE}_MAINNET=1` is set,
    // which is precisely the state an operator is in while trying to go live. See
    // `CexArming::remedy`.
    let arming = with_other_live_accounts(
        cex_arming(*venue, *mainnet, exec_live),
        slug,
        other_live_accounts(slug, venue_arming, live_venues),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %slug, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; {slug} feed subscribed (klines + quote/trade/book \
             pump) — ⚠ EXEC IS LIVE ON {network}: orders from this mount are REAL orders \
             against the {slug} {network} account whose credentials were found",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %slug, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; {slug} feed subscribed (klines + quote/trade/book \
             pump) — but EXEC IS PAPER for {slug}, so `build_node` mounted the paper \
             exchange and NO ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE. ⚠ TWO causes \
             reach this line and they need DIFFERENT fixes: the credentials are ABSENT \
             from the store this box reads, or they are PRESENT and the startup preflight \
             REFUSED them — its `[FAIL] credentials ({slug})` line above names what the \
             venue answered (an expired or revoked key lands here with both keys in the \
             store). Read that line before acting on this one. If they are absent: \
             {remedy}",
        ),
    }
}
