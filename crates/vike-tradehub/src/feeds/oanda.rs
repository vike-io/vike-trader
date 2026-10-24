//! OANDA's feed arm: the two-lane `DataClient` built through `FeedCtors`, then the arming.

use std::sync::Arc;
use std::time::Duration;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};
use crate::venue_arming::arming::{
    data_only_arming, mounts_exec_live, oanda_arming, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// OANDA's feed: the two-lane `DataClient` built through the [`FeedCtors`] seam (streamed quotes +
/// polled candle closes) on the venue-agnostic `CoreLaneSink`, with a bar subscription per
/// distinct `(symbol, interval)` and a quote subscription per distinct symbol across this venue's
/// mounts. Any subscription failure refuses the mount, except a CAP refusal of one symbol's quote stream
/// (that symbol runs without quotes and the start goes on) and a TRANSIENT one, which is asked again
/// (`subscribe_quotes_surviving`).
///
/// The feed is held as a [`QuoteFeed`] (as alpaca's): a tap in front of the core's sink records the last
/// price per symbol, and the [`QuoteFeed::watcher`] subscribes the QUOTE lane of a symbol the control door
/// admits (decision 0174), so a foreign OANDA position has a mark. One `/pricing/stream` per symbol (the
/// bridge's `market_feed` module doc), so the venue's session cap bounds the admitted symbols (UNMEASURED).
pub(super) fn wire_oanda_feed(
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    // The credentialed-data shape again, but over a client that owns NO socket: quotes
    // arrive on a chunked-HTTP `/pricing/stream` reader thread and bars on a candles-REST
    // POLL thread, both spawned by the client's own `FeedRegistry`. Everything lands on
    // the venue-agnostic `CoreLaneSink` (through `wrap` like every other arm),
    // NOT a `MakerSink`: this venue's poll lane emits REAL `close_bar`s on the lossless
    // lane, so the margin/drawdown watchdogs sweep on the venue's own candle closes and
    // nothing needs synthesizing (the ctrader arm's synth exists only because cTrader
    // never fires a close).
    //
    // ⚠ MOUNT-FAILURE SEMANTICS, and they are ctrader's OPPOSITE — deliberately.
    // The client builds NO socket at the gatewayhub's open and each `subscribe_*` only starts a
    // thread that then dials with its own stop-aware backoff, so a bad token or an
    // unreachable host surfaces as a reconnect-looping feed in the log, never a mount
    // error. That is the alpaca shape and it is the right one here: there is nothing
    // SYNCHRONOUS to fail. The credential half is the gatewayhub's refusal of the credentialed stream
    // (the account's `API_KEY` + `ACCOUNT_ID` fields), which reaches this arm as the first
    // `subscribe_*`'s error and refuses the mount.
    let (mut feeds, tap) = dial_oanda(handle, wrap, make)?;
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
    // each is its OWN poll thread at its own cadence, so unlike ctrader two intervals
    // genuinely both fire — and one QUOTE subscription per distinct symbol. The HL arm's
    // dedup shape; an interval the venue has no candle series for is the bridge's refusal of the bar
    // subscription (`oanda_plan` no longer holds the table), and refuses the mount here.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.token_id, &c.interval));
            feeds.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                format!("oanda subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
            })?;
        }
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            subscribe_quotes_surviving(&mut feeds, &c.token_id, make)?;
        }
    }
    Ok(QuoteFeed::new(feeds, tap))
}

/// How many times the arm asks for one symbol's quote stream before a TRANSIENT refusal ends the start: three
/// tries, [`QUOTE_RETRY_PAUSE`] apart, so ten seconds at the most.
const QUOTE_TRIES: u32 = 3;

/// The wait between two tries of one symbol's quote stream.
const QUOTE_RETRY_PAUSE: Duration = Duration::from_secs(5);

/// Ask for `symbol`'s quote stream, surviving the two refusals that are about the KEY and the moment, not
/// the venue (decision 0193):
///
/// * **A TRANSIENT one is asked again**, up to [`QUOTE_TRIES`] tries, [`QUOTE_RETRY_PAUSE`] apart (a wait that
///   ends when the process is asked to stop, `FeedCtors::pause`). The tradehub restarts last in a deploy, and
///   its boot-time subscribe can meet the gatewayhub still joining the streams the recorder dropped 30 to 90
///   seconds after the old tradehub let them go (`gatewayhub`'s session guard refuses a subscribe that waited
///   too long behind a join, as transient); the joins are done by the next try. Without this a running OANDA
///   demo account turned that into a plain failed start and a supervisor restart. Every try refused still
///   ends the start, loudly, with the last refusal.
/// * **A CAP refusal** (the datahub's or the gatewayhub's stream ceiling) costs THAT key's quote stream and
///   nothing else: this start would otherwise end for every venue's engines on one symbol too many (the core
///   is up and cannot drop a running account's engine, so the arm's `Err` is `EXIT_OPERATOR_NEEDED`). The
///   mount keeps its candle closes, which are REST polls the ceiling does not count; only its quote lane is
///   lost, and a retry would meet the same ceiling.
///
/// Any other refusal is returned as it came.
fn subscribe_quotes_surviving(
    feeds: &mut Tracked,
    symbol: &str,
    make: &dyn FeedCtors,
) -> Result<(), String> {
    let mut tries = 0;
    loop {
        tries += 1;
        let Err(e) = feeds.subscribe_quotes(symbol) else { return Ok(()) };
        let refusal = format!("oanda subscribe_quotes {symbol}: {e:?}");
        if vike_market_stream::market::is_cap_refusal(&refusal) {
            tracing::warn!(
                symbol,
                reason = %refusal,
                "oanda: the QUOTE stream of this mount's symbol was refused by a stream ceiling (a cap, \
                 not a link): the mount runs on its candle closes WITHOUT quotes, and every other \
                 symbol is unaffected. Free a stream (stop a strategy or a recorded symbol that holds \
                 one) and restart this tradehub"
            );
            return Ok(());
        }
        if tries < QUOTE_TRIES && vike_market_stream::market::is_transient_refusal(&refusal) {
            tracing::warn!(
                symbol,
                reason = %refusal,
                try_number = tries,
                of = QUOTE_TRIES,
                "oanda: the QUOTE stream of this mount's symbol was refused as TRANSIENT (the gatewayhub is \
                 still busy with the streams an earlier session released): asking again in a few seconds"
            );
            if !make.pause(QUOTE_RETRY_PAUSE) {
                continue;
            }
        }
        return Err(refusal);
    }
}

/// OANDA's PRICE-ONLY feed, for a box that mounts no OANDA market (the twin of
/// `deribit::wire_deribit_price_feed`): the dial and the price tap, nothing subscribed. The control door
/// subscribes a pair's quote lane through its [`QuoteFeed::watcher`] when an order names an instrument the
/// account was not started on (decision 0174); without it an `Admits` venue with no mount answers
/// `this node runs no price feed for oanda` to every foreign pair. The dial is the mounted arm's (a
/// credentialed stream at the gatewayhub over the practice session of the venue's default `demo` account),
/// so the gatewayhub's refusal of an account with no usable credentials arrives at the first subscription,
/// as the door's refusal of that order, never at wiring.
pub(super) fn wire_oanda_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    let (feeds, tap) = dial_oanda(handle, wrap, make)?;
    Ok(QuoteFeed::new(feeds, tap))
}

/// The dial both OANDA arms start from, and the price tap in front of its sink.
fn dial_oanda(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<(Tracked, Arc<PriceTap>), String> {
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    // The tap in front of the core's sink only RECORDS the last price per symbol (every verb is forwarded
    // unchanged), so the control door can tell whether a symbol it admitted has a price yet. It ages a
    // quote from the VENUE's time: the bridge's REST first price arrives up to its bound old, and aged
    // from arrival it would stay a door price after the core's price-only sweep calls it stale.
    let tap = Arc::new(PriceTap::venue_clocked());
    let sink = wrap(Arc::new(TapSink::new(core, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    // Constructed through the [`FeedCtors`] seam (the deribit arm's shape): production
    // is the strategy-lane dial — a credentialed stream at the gatewayhub, over the PRACTICE session
    // of the venue's default `demo` account — and the deterministic splice test
    // (`src/tradehub_cli/tests/feed_splice.rs`) substitutes a scripted constructor HERE, which is
    // what lets it drive this very arm with scripted frames on a weekend the real venue is closed for.
    let feeds = Tracked::new(make.client("oanda", None, sink)?);
    Ok((feeds, tap))
}

/// OANDA's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_oanda_arming(
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = mounts_exec_live("oanda", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("oanda") && !exec_live {
            // The DECLARED exception: paper-by-declaration, not the gate disagreement the
            // ordinary remedy reports. Guarded on `!exec_live` so a build where the withhold
            // failed to hold still announces `EXEC IS LIVE` honestly.
            data_only_arming(oanda_arming(false), "oanda")
        } else {
            with_labelled_mount_remedy(oanda_arming(exec_live), "oanda", mount_accounts)
        },
        "oanda",
        other_live_accounts("oanda", venue_arming, live_venues, mount_accounts),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; oanda feed subscribed (streamed L1 quotes + POLLED \
             candle closes, both Bearer-authed) — ⚠ EXEC IS LIVE ON {network}: orders \
             from this mount are REAL orders against the oanda {network} account whose \
             token was found",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; oanda feed subscribed (streamed L1 quotes + POLLED \
             candle closes, both Bearer-authed) — but EXEC IS PAPER for oanda and NO \
             ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
        ),
    }
}
