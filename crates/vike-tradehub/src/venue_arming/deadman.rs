//! The two dead-man switches the LIVE start arms — pure folds from `policy` onto the core's config.

use std::time::Duration;

#[cfg(doc)]
use super::warn_deadman_absent;
use crate::VenuePlan;

// -------------------------------------------------------------------------------------------
// The dead-man switches the LIVE start arms — pure folds from `policy` onto the core's config
// -------------------------------------------------------------------------------------------

/// The file spelling of the dead-man action → the core's own type.
///
/// An extension trait rather than a `From` impl because BOTH types are foreign here (the orphan
/// rule), and here rather than in `vike-config` because that crate sits below `vike-core` and must
/// not depend on it — this binary is the crate that depends on both, so the mapping is its job
/// (`vike_config::DeadManActionSetting`'s doc says the same from the other side). The `match` is
/// exhaustive on the file side, so a variant added there is a compile error here rather than a
/// spelling that silently maps to the other action.
pub(crate) trait DeadManActionToCore {
    fn to_core(self) -> vike_core::DeadManAction;
}

impl DeadManActionToCore for vike_config::DeadManActionSetting {
    fn to_core(self) -> vike_core::DeadManAction {
        match self {
            vike_config::DeadManActionSetting::CancelAllAndHalt => {
                vike_core::DeadManAction::CancelAllAndHalt
            }
            vike_config::DeadManActionSetting::CancelAll => vike_core::DeadManAction::CancelAll,
        }
    }
}

/// The dead-man switch the LIVE start arms, folded from `policy.deadman_timeout_ms` and
/// `policy.deadman_action` — the whole of
/// M4's wiring surface, as one pure function so the fold can be pinned without racing a timer.
///
/// `None` when the key is ABSENT (`Policy::deadman_timeout_ms` is `None`) and when the operator
/// wrote `deadman_timeout_ms = 0` (`Some(vike_config::DEADMAN_DISABLED_MS)`) — the two are OFF
/// alike, and only the WARNING beside this call ([`deadman_absent_warning`]) tells them apart.
/// `vike_core::CoreConfig::deadman` being `None` is what makes the core build no `DeadMan`, arm no
/// sweep timer and touch nothing on the fold — the switch's "off" is the ABSENCE of the config,
/// not a zero inside it (the core would clamp a zero timeout to 1 ms and trip on the first quiet
/// millisecond). Every other value arms it at exactly that many milliseconds; the file edge
/// (`vike_config::Policy::apply`) has already refused the sub-second and multi-day values, so
/// nothing is clamped here either. ⚠ For one morning `None` was unreachable from this function:
/// the key was a `u64` defaulting to 60 s, and this doc said "`None` exactly when the operator
/// wrote 0". The key's own doc (`vike_config::Policy::deadman_timeout_ms`) records why that was
/// reversed the same day — the switch observes SILENCE, not the connection, so an armed default
/// halted every session-bounded venue at every close.
///
/// `halt_file` is `vike_boot::halt::halt_path_from_env()` — resolved by THIS binary, not by
/// `vike-core` (env reads stay in binaries, and `vike-core` deliberately carries no
/// `vike-bridge-core` edge). It is the SAME sentinel a manual `touch HALT` writes and every venue's
/// `ExecActor` submit boundary checks, so the automatic trip and the operator's hand reach one
/// file; the resolver is memoized, so the path the switch will write is the path the start's own
/// halt report already printed.
///
/// ⚠ Called from `live_runtime_with` ONLY. The daemon's `start_paper` arm does not read these keys,
/// by ruling — see `vike_config::Policy::deadman_timeout_ms` for why a rehearsal that halts itself
/// over a quiet minute is not wanted. ⚠ That is the live GATE, not exec actually arming: the call
/// site runs before `assemble_runtime` decides per venue, so a live-gate run whose every exec is still
/// paper (no active non-paper `account` row, `data_only`) IS armed and a trip there
/// writes the process's real HALT sentinel. The key's doc states that shape and flags the
/// alternative (keying on `vike_tradehub_core::armed_live_venues`) as an owner decision not taken here.
/// `vike-app`'s live start did not construct it either, nor did `crates/vike-run/src/bin/ibkr_mount.rs`
/// (deleted 2026-09-28); `docs/ops/kill-switches.md` carries those residuals.
pub(crate) fn deadman_config_from_policy(
    policy: &vike_config::Policy,
) -> Option<vike_core::DeadManConfig> {
    let timeout_ms = match policy.deadman_timeout_ms {
        None | Some(vike_config::DEADMAN_DISABLED_MS) => return None,
        Some(ms) => ms,
    };
    Some(vike_core::DeadManConfig {
        timeout: Duration::from_millis(timeout_ms),
        action: policy.deadman_action.to_core(),
        halt_file: Some(vike_boot::halt::halt_path_from_env()),
    })
}

/// **Does THIS daemon subscribe a lane that can disclose a dead link for the venue?** — the third
/// fact the link dead-man's arming decision needs, and the one neither the `policy` rows nor
/// `vike_venues::link_deadman_default` can answer.
///
/// ⚠ **Why it exists at all: the per-venue table answers a question about the ADAPTER, and this
/// daemon does not subscribe every lane an adapter has.** `link_deadman_default` says a venue's
/// bridge discloses a disconnect and cites the emitters; on binance/aster/bybit/okx there are two,
/// both on a BOOK socket, and this daemon subscribes exactly one of them — the HFT quote/trade/book
/// tick pump, never `subscribe_depth` (whose `l2_snapshot` verb is a default no-op in every sink
/// this daemon owns — [`crate::feeds`]'s `VenuePlan::Cex` arm says so in its own comment). So this
/// fold is what keeps the report honest in BOTH directions: without it the start once logged
/// `LINK DEAD-MAN ARMED for binance` while nothing could reach the latch — a false claim about an
/// automatic stop, the exact class `docs/ops/kill-switches.md` opens by warning about — and a
/// venue whose only emitter this daemon stopped subscribing would silently go back to that state.
///
/// One arm per [`VenuePlan`] variant, each read from the feed wiring it describes.
///
/// ⚠ **Every venue of this daemon rides the datahub's strategy lane** (wave 2: each feed is a
/// `vike_datahub_client::strat::StratFeed`), and that LINK discloses under its own source
/// (`vike_venues::link_deadman::STRAT_LINK_STREAM`) for every venue, whatever its bridge does. So no plan is
/// "silent" any more: the question per venue is whether its OWN socket also discloses
/// ([`Self::Discloses`]) or only the two hub links do ([`Self::StratLinkOnly`]: the datahub's and the account's trade link to the gatewayhub).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdapterLinkDisclosure {
    /// This start subscribes a lane that hands `StreamStatus` to a sink the core reads, so a link
    /// death on it reaches `vike_core`'s latch. `lane` names the subscription for the startup line.
    Discloses { lane: &'static str },
    /// The venue's own sockets disclose nothing this daemon hears (its bridge calls
    /// `LiveDataSink::stream_status` nowhere), but two LINKS do: the strategy-lane link to the datahub
    /// (`gateway-strat`) and the account's trade link to the gatewayhub (`gateway-trade`, disclosed for every
    /// mounted key of every account). Arming is per VENUE, not per source, so an outage of EITHER longer than
    /// the grace trips the switch: a datahub restart, and a gatewayhub restart, over the grace. `lane` is told to
    /// the operator verbatim.
    StratLinkOnly { lane: &'static str },
}

impl AdapterLinkDisclosure {
    /// **Does the link dead-man arm for `venue` on this disclosure?** The ONE fold both the config and the
    /// startup report read.
    ///
    /// * [`Self::Discloses`]: when the venue's table row is `Armed` (the venue trades continuously) —
    ///   binance, aster, bybit, okx and polymarket, byte-identical to before. A `SessionBounded` row (oanda,
    ///   ig) stays OFF: its own socket can drop at every weekend close, and the switch arms per VENUE, not per
    ///   source, so arming it for the datahub link would arm the weekend close with it.
    /// * [`Self::StratLinkOnly`]: when the row is `Inert` (hyperliquid, deribit, alpaca, ctrader). Its own
    ///   socket can never open a window, so arming the venue arms the two LINKS: the datahub's
    ///   (`gateway-strat`) and the account's trade link to the gatewayhub (`gateway-trade`;
    ///   `vike_core::LinkDeadMan::observe` keys a window by source but ARMS by venue alone, and the trade link
    ///   discloses a `Disconnected` for every mounted key). An outage of either past the grace cancels the
    ///   venue's resting orders and halts, as it does on the venues that were armed before, so a gatewayhub
    ///   restart longer than the grace halts trading on these four exactly as it does on the CEX venues. An
    ///   `Armed` or `SessionBounded` row mounted over a plan that discloses only the strat link is NOT armed
    ///   (the row's own socket is the question the row answers, and nothing here subscribes it).
    pub(crate) fn arms(self, venue: &str) -> bool {
        let row = vike_venues::link_deadman_default(venue);
        match self {
            AdapterLinkDisclosure::Discloses { .. } => row.is_on(),
            AdapterLinkDisclosure::StratLinkOnly { .. } => {
                matches!(row, vike_venues::link_deadman::LinkDeadMan::Inert { .. })
            }
        }
    }
}

/// The per-plan reading behind [`AdapterLinkDisclosure`]. Pure and total over [`VenuePlan`], so a new
/// variant has to answer the question before it compiles.
///
/// Formerly `mount_link_disclosure`, renamed in the shape-and-names change (decision 0166).
pub(crate) fn adapter_link_disclosure(plan: &VenuePlan) -> AdapterLinkDisclosure {
    match plan {
        // ⚠ The CEX arm subscribes TWO lanes and exactly ONE of them discloses. The kline `Feeds`
        // lane does not and never has: it rides `vike_bridge_core::market_pump`, which carries no
        // link disclosure at all (`crates/bridges/binance/src/family/trades.rs`'s module doc states
        // it for the trade lane in the same words). The quote/trade/book PUMP does: it owns no
        // `LiveDataSink` — it takes a bare `vike_exec::TickSender` — and pushes a
        // `vike_model::FeedStatus` straight onto it, which is the same `Ingest::StreamStatus` arm a
        // sink-side disclosure reaches. That is the lane this arm rests on, and it is the venue's
        // BOOK socket rather than a side channel: the same `@depth`-class stream `depth_main`
        // reads, through the other seam.
        //
        // ⚠ **`subscribe_depth` is still NOT the answer here and must not be "added for
        // completeness".** It would open a SECOND socket to the SAME stream and maintain a SECOND
        // copy of the same book, to obtain a signal the pump already has — and deliver it into
        // `l2_snapshot`, a default no-op in every sink this daemon owns. See the pump's own
        // `disclose_link` for the argument.
        VenuePlan::Cex { .. } => AdapterLinkDisclosure::Discloses {
            lane: "the quote/trade/book tick pump (crates/bridges/binance/src/family/depth.rs's \
                   md_main and its bybit/okx twins), which discloses its transport state straight \
                   onto the core tick lane — one Disconnected per outage, one Live on the first \
                   frame back. NOT the kline lane, which discloses nothing, and NOT the DOM depth \
                   feed, which this daemon does not subscribe",
        },
        // The one arm that DOES: `subscribe_book` on every seated token, onto a `vike_tradehub_core::
        // MakerSink` whose `stream_status` forwards to its inner `vike_tradehub_core::CoreLaneSink`.
        VenuePlan::Polymarket => AdapterLinkDisclosure::Discloses {
            lane: "subscribe_book on every seated token, onto a MakerSink that forwards \
                   stream_status to its inner CoreLaneSink",
        },
        // Both of these reach a `CoreLaneSink` directly and their feeds DO emit GapStart — so the
        // start is not what stops them; `link_deadman_default` is (both are SessionBounded).
        VenuePlan::Oanda => AdapterLinkDisclosure::Discloses {
            lane: "the pricing-stream + candles-poll lanes onto a CoreLaneSink",
        },
        VenuePlan::Ig => AdapterLinkDisclosure::Discloses {
            lane: "the Lightstreamer quote/candle subscriptions onto a CoreLaneSink",
        },
        // The rest are `LinkDeadMan::Inert` in the table for the same underlying reason — their
        // bridges call `LiveDataSink::stream_status` nowhere — so no socket of THEIRS can disclose a dead
        // link. What can: the strategy-lane link to the datahub, which every one of these feeds rides
        // (`StratFeed` discloses a `GapStart` under `STRAT_LINK_STREAM` for each held symbol when the
        // session drops, and closes it with a `Live` when it is back or the feed is released).
        VenuePlan::Hyperliquid(_) | VenuePlan::Alpaca | VenuePlan::Ctrader | VenuePlan::Deribit => {
            AdapterLinkDisclosure::StratLinkOnly {
                lane: "the strategy-lane link to the datahub (the `gateway-strat` source: a \
                       GapStart per held symbol when the datahub session drops, a Live when it is \
                       back) AND the account's trade link to the gatewayhub (the `gateway-trade` \
                       source; the switch arms per venue, not per source). The venue's own bridge \
                       calls LiveDataSink::stream_status nowhere, so no socket of the venue's can \
                       open a window",
            }
        }
    }
}

/// How often the core sweeps the margin call and the drawdown latch of the PRICE-ONLY engines off the
/// clock. Half the 60 s price age (`feeds/price_tap.rs`'s `MAX_PRICE_AGE`) and the same cadence as the
/// 15 s waker the live core already runs for its stuck-order watchdog, so it adds no wake-ups; a mounted
/// venue is swept once per closed bar, a minute or more.
pub(crate) const PRICE_ONLY_SWEEP: Duration = Duration::from_secs(30);

/// **The link dead-man's reading of a PRICE-ONLY venue** (one the door admits symbols for and no mount
/// names, `crate::feeds::price_only_venues`): the on-demand pump of an admitted symbol is the same
/// quote/trade/book tick pump a mount subscribes (the CEX arm of [`adapter_link_disclosure`]), and
/// Hyperliquid's, deribit's, ctrader's, ibkr's and dukascopy's price lane is a quote subscription whose own socket discloses
/// nothing, so only the two hub links speak for it (the `StratLinkOnly` arm); ig's is its Lightstreamer quote lane
/// and oanda's its pricing stream (`Discloses`, as their mounts'), and their `SessionBounded` rows keep them out
/// of the armed set whichever they are. The venue is watched only while the CORE says it
/// holds an admitted symbol or a live order (`vike_core::PriceOnlyConfig`): nothing is subscribed otherwise, so
/// "silent" means nothing.
///
/// ⚠ **ibkr's reading, and why it is not `SessionBounded`.** `vike_venues::link_deadman_default("ibkr")` is
/// `Inert`: `crates/bridges/vike-ibkr/src/market_feed/pump.rs` calls `stream_status` with a `Stale` only, never a
/// `GapStart` (a stock market also closes every day, which is what `SessionBounded` would bound had the pump a
/// `GapStart` to bound). The ONE ibkr emitter of a `GapStart` is the price stream's supervisor
/// (`crates/bridges/vike-ibkr/src/quote_handle.rs`'s `Shared::supervise`): a data connection that dies (the daily
/// logoff, a lost link) is disclosed on the `quotes` lane of every held symbol, a venue-source window that the
/// re-dial's `Live` closes, so an outage past the grace trips ibkr like a trade-link outage does (the daily logoff
/// opens both windows every night, each about as long as the gateway is away). What also speaks for ibkr is the two
/// hub links and the IB Gateway's own link: the link watchdog (`LinkMonitor`, 5 minutes with no link to IBKR) ends
/// the exec session, the host tears the adapter down, and that reaches the core as the account's trade-link
/// `Disconnected` (`gateway-trade`). While the data connection is down, or its stream is one IB refused, the
/// symbols' prices go stale: the price tap's age bound ends them ("none yet") and the core's price-only sweeps skip
/// a price older than that bound - fail-closed.
///
/// ⚠ **dukascopy's reading, and why it is not `Discloses`.** `vike_venues::link_deadman_default("dukascopy")` is
/// `Inert`, and the price lane keeps it true: `crates/vike-gatewayhub/src/data/dukascopy_quotes.rs` hands the sink
/// quotes and a real-time label and NEVER a `GapStart` or a `Live`, because an FX weekend close and a dead sidecar
/// are both silence on a JForex tick stream and a window opened by either would have to be told apart by a clock.
/// What speaks for the venue is the two hub links. A sidecar that dies alone is not reported to the core as a link
/// state by anything on this lane (the exec client turns a later order on its account into a terminal
/// `bridge unavailable` rejection), and its pairs' prices simply age out (the price tap's bound ends them in
/// "none yet", and the core's price-only sweeps skip a price older than it): fail-closed, but not DETECTED as a
/// link death.
pub(crate) fn price_only_link_disclosure(venue: &str) -> AdapterLinkDisclosure {
    if venue == "ig" {
        // The mounted arm's reading (`adapter_link_disclosure`'s `VenuePlan::Ig`): the Lightstreamer quote
        // lane discloses onto a CoreLaneSink. The venue's row is `SessionBounded` (a session end at a
        // weekend close reads as a dead link), so `arms` is false for it either way: a price-only ig is
        // never in the armed set, and the core's holdings gate and sweep still read it.
        AdapterLinkDisclosure::Discloses {
            lane: "the on-demand Lightstreamer quote lane of each admitted epic onto a CoreLaneSink, \
                   watched only while the venue holds an admitted symbol or a live order",
        }
    } else if venue == "oanda" {
        // The mounted arm's reading (`adapter_link_disclosure`'s `VenuePlan::Oanda`): the pricing stream
        // discloses onto a CoreLaneSink. The venue's row is `SessionBounded` (a weekend close drops its own
        // socket), so `arms` is false for it either way: a price-only oanda is never in the armed set.
        AdapterLinkDisclosure::Discloses {
            lane: "the on-demand pricing-stream of each admitted pair onto a CoreLaneSink, watched only \
                   while the venue holds an admitted symbol or a live order",
        }
    } else if matches!(venue, "hyperliquid" | "deribit" | "ctrader" | "ibkr" | "dukascopy") {
        AdapterLinkDisclosure::StratLinkOnly {
            lane: "the strategy-lane link to the datahub (a GapStart per held symbol when the datahub \
                   session drops) AND the account's trade link to the gatewayhub, while the venue \
                   holds an admitted symbol or a live order",
        }
    } else {
        AdapterLinkDisclosure::Discloses {
            lane: "the on-demand quote/trade/book tick pump of each admitted symbol (the mounts' \
                   pump, subscribed by the control door), watched only while the venue holds an \
                   admitted symbol or a live order",
        }
    }
}

/// The core's price-only configuration for `venues` (`crate::feeds::price_only_venues`): the sweep
/// cadence, the price age bound the sweeps share with the control door's price tap
/// (`crate::feeds::price_tap::MAX_PRICE_AGE`), the network each feed prices from
/// ([`price_source_tiers`]) and the set the link dead-man gates on holdings. `hl_feed_tier` is the tier
/// `feed_tier` gave Hyperliquid's price-only feed (`None` when it is not wired). `None` — nothing armed,
/// nothing different on the core — when no venue is price-only.
pub(crate) fn price_only_config(
    venues: &[String],
    hl_feed_tier: Option<vike_model::AccountTier>,
) -> Option<vike_core::PriceOnlyConfig> {
    (!venues.is_empty()).then(|| vike_core::PriceOnlyConfig {
        venues: venues.iter().cloned().collect(),
        sweep_interval: PRICE_ONLY_SWEEP,
        max_price_age: crate::feeds::price_tap::MAX_PRICE_AGE,
        price_source: price_source_tiers(venues, hl_feed_tier),
    })
}

/// **The network each price-only feed DIALS, as the account tier that trades there** (`Live` = mainnet,
/// `Demo` = testnet), for the venues whose feed network is known to differ from some account's exec
/// network: the core keeps a price-only engine on a real network (`tier != Paper`) at another tier out of
/// the margin call and the drawdown latch (`vike_core::PriceOnlyConfig::price_source`).
///
/// * `deribit`: the public MAINNET host, always (`wire_deribit_price_feed`; exec signs on the testnet and
///   no flag or tier reaches deribit mainnet exec), so every real deribit account is on the other
///   network. Which host is dialled is the gatewayhub's and is not changed here.
/// * `hyperliquid`: the tier its feed was wired on (`hl_price_network`/`feed_tier`: mainnet when a `live`
///   account is armed), so only an account beside a live one on a straddling venue is crossed.
/// * `ctrader` and `ig`: `Demo`, always. The gatewayhub opens the ONE credentialed stream of the venue as its
///   DEFAULT `demo` account (`vike_secrets::account_directory::AccountDirectory::data_feed_credentials`:
///   `demo`, or the `paper` row's demo login, and never a `live` key whatever accounts exist), so a `live`
///   account of either venue is priced off the demo broker's quotes and stays out of the margin call and the
///   drawdown latch (its protection is the stuck-order ladder and the dead-man).
/// * every other venue is ABSENT, i.e. trusted: the CEX feeds are mainnet by design over a `demo` account
///   too ("demo exec over mainnet prices", the arm comments of `crate::feeds`), an owner-accepted shape
///   this change leaves alone. Adding a venue here is one arm.
pub(crate) fn price_source_tiers(
    venues: &[String],
    hl_feed_tier: Option<vike_model::AccountTier>,
) -> std::collections::BTreeMap<String, vike_model::AccountTier> {
    venues
        .iter()
        .filter_map(|v| match v.as_str() {
            "deribit" => Some((v.clone(), vike_model::AccountTier::Live)),
            "ctrader" | "ig" => Some((v.clone(), vike_model::AccountTier::Demo)),
            "hyperliquid" => hl_feed_tier.map(|t| (v.clone(), t)),
            _ => None,
        })
        .collect()
}

/// **The LINK dead-man the live start arms** (M13) — the fold from `policy.link_deadman_grace_ms`, the
/// per-venue table, the venues this daemon actually starts AND the lanes it subscribed for each,
/// as one pure function so the whole decision can be pinned without racing a timer.
///
/// `None` — the switch NOT constructed — in exactly two cases, and they are different facts:
/// the operator wrote `link_deadman_grace_ms = 0` (`vike_config::Policy::
/// link_deadman_grace_ms_effective` resolves that to `None`, and an ABSENT key to the armed
/// default), or NOT ONE started venue is armed ([`AdapterLinkDisclosure::arms`]: an `Armed` table row
/// whose own lane discloses here, or an `Inert` row whose only voice is the strategy-lane link to the
/// datahub). The second is the ordinary state of an FX-only daemon (oanda and ig are `SessionBounded`);
/// it was ALSO the state of a CEX-only one until the tick pumps grew a
/// disclosure (2026-09-06), which is the reach gap `docs/decisions/0038-the-dead-man-observes-the-
/// connection-not-silence.md` recorded and its "what would reopen this" clause named.
/// [`link_deadman_arming_report`] is what tells the operator which of the three happened, per
/// venue.
///
/// ⚠ **The datahub link (wave 2).** Every feed rides the datahub's strategy lane, and the link discloses
/// under its own source for every venue, so a datahub outage past the grace is a total price blackout the
/// switch must see. The default rule for the four venues that used to be unwatched (hyperliquid, deribit,
/// alpaca, ctrader — `Inert` rows, whose own sockets disclose nothing) is therefore ARMED, the same grace
/// and the same action as the CEX venues and polymarket, which are byte-identical to before. oanda and ig
/// stay OFF with the report line saying so: the switch arms per venue and a `SessionBounded` venue's own
/// socket would be armed with it (a per-source set in `vike_core::LinkDeadManConfig` is the cure, a
/// measured-crate change). A datahub restart is now a trading event on every armed venue after the grace.
///
/// ⚠ **And the gatewayhub's trade link counts too.** The switch arms per VENUE, so arming one of those four
/// for the datahub link arms every source that venue's keys disclose, and the trade link to the gatewayhub
/// (`gateway-trade`) is one: a gatewayhub restart longer than the grace trips `CancelAllAndHalt` on
/// hyperliquid, deribit, alpaca and ctrader as it already did on the CEX venues; the halt is THAT venue's
/// alone and lifts when its links are Live again (decision 0180). The startup line says both links
/// ([`link_deadman_arming_report`]).
///
/// ⚠ **An empty venue set is never constructed as `Some`.** A `LinkDeadManConfig` whose `venues`
/// is empty would arm a timer, contribute a waker cadence and forfeit journal replay to watch
/// nothing — the "a mechanism exists" claim `docs/ops/kill-switches.md` opens by warning about,
/// wearing a config struct.
///
/// `action` is `deadman_action`, SHARED with the silence switch: what a trip DOES is the same
/// question for both, and a second key would be a second answer to it. ⚠ There is no sentinel path
/// here (decision 0180): the silence switch's halt is node-wide and writes the one sentinel; this
/// switch's halt is the dead venue's alone and writes none.
///
/// ⚠ **`mounted_venues` also carries the PRICE-ONLY venues** (`price_only_link_disclosure`, appended by
/// `live_runtime_with`): this fold cannot tell them apart and must not, because the core does. A venue
/// that is also in `vike_core::CoreConfig::price_only` is acted on only while it holds an admitted symbol or
/// a live order, so arming it here costs an idle box nothing.
///
/// ⚠ Called from `live_runtime_with` ONLY — the daemon's `start_paper` arm constructs neither
/// switch, by the same ruling and for the same reason (`vike_config::Policy::deadman_timeout_ms`
/// argues it once). ⚠ And that is the live GATE, not exec actually arming: a live-gate run whose
/// every exec is still paper carries this switch too, and a trip there halts that venue's (paper)
/// engines — no sentinel is written, so a rehearsal never touches the process's real HALT file.
pub(crate) fn link_deadman_config_from_policy(
    policy: &vike_config::Policy,
    mounted_venues: &[(String, AdapterLinkDisclosure)],
) -> Option<vike_core::LinkDeadManConfig> {
    let grace_ms = policy.link_deadman_grace_ms_effective()?;
    let venues: std::collections::BTreeSet<String> = mounted_venues
        .iter()
        .filter(|(v, disclosure)| {
            // The fold is `AdapterLinkDisclosure::arms`: a disclosing venue arms when its table row says
            // so, a venue only the datahub link can speak for arms when its own socket can never open a
            // window. An armed venue whose lanes could disclose nothing here would put a name in this set
            // that the latch can never hear about, which is the "a mechanism exists" claim wearing a
            // config struct.
            disclosure.arms(v)
        })
        .map(|(v, _)| v.clone())
        .collect();
    if venues.is_empty() {
        return None;
    }
    Some(vike_core::LinkDeadManConfig {
        grace: Duration::from_millis(grace_ms),
        action: policy.deadman_action.to_core(),
        venues,
    })
}

/// **One line per started venue**, saying whether the link dead-man armed there and why — returned
/// rather than logged so the DECISION is testable with no subscriber (the
/// [`deadman_absent_warning`] / `vike_tradehub_core::unaddressable_accounts_message` idiom).
///
/// Why per venue and not one summary line: the FOUR ways a venue ends up unarmed are not the same
/// fact and an operator must be able to tell them apart. A venue can be off because its market has
/// SESSIONS (`vike_venues::LinkDeadMan::SessionBounded` — the conservative default, flippable by an
/// observation), because its bridge discloses no disconnect at all (an `Inert` row is ARMED only for the datahub link, see [`AdapterLinkDisclosure::arms`])
/// (`vike_venues::link_deadman::LinkDeadMan::Inert` — a declared residual nothing can fix from the `policy` rows),
/// because THIS START subscribes no venue-side lane that would carry one ([`AdapterLinkDisclosure::StratLinkOnly`] on an `Armed` or `SessionBounded` row —
/// a fact about the daemon rather than the venue, and the only one of the four a code change here
/// could fix), or because the operator wrote `link_deadman_grace_ms = 0`. A summary saying "3 of 5
/// venues armed" hides all four behind a count.
///
/// ⚠ **The mount-lane case is checked BEFORE the armed line is printed, and that ordering is the
/// whole fix.** The first version of this function printed `LINK DEAD-MAN ARMED for binance …
/// cancels this venue's resting orders` on a daemon where nothing could ever reach the latch for
/// binance, because it consulted the venue table alone. A false promise about an automatic stop is
/// worse than no line at all.
pub(crate) fn link_deadman_arming_report(
    policy: &vike_config::Policy,
    mounted_venues: &[(String, AdapterLinkDisclosure)],
) -> Vec<String> {
    let grace = policy.link_deadman_grace_ms_effective();
    // ⚠ The SAME class as [`deadman_absent_warning`] below, found by the sweep that fixed it: the
    // `(None, _, _)` arm told an operator to DELETE A LINE from a file nothing reads any more.
    // `vike_config::remedy` renders the instruction: the way back to the default grace is
    // `config unset`, which removes the row so the key resolves to
    // `vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS` again (it used to be a `config set` of that
    // number, when no verb removed a row).
    let grace_remedy = vike_config::SettingsSection::Policy.write_remedy("link_deadman_grace_ms");
    mounted_venues
        .iter()
        .map(|(venue, disclosure)| {
            let row = vike_venues::link_deadman_default(venue);
            match (grace, disclosure.arms(venue), row.off_reason(), disclosure) {
                (None, _, _, _) => format!(
                    "LINK DEAD-MAN off for {venue}: `link_deadman_grace_ms = 0` in {} turns it \
                     off for EVERY venue. {} to restore the default grace",
                    grace_remedy.holder(),
                    grace_remedy.unset_clause()
                ),
                (Some(ms), true, _, AdapterLinkDisclosure::Discloses { lane }) => format!(
                    "LINK DEAD-MAN ARMED for {venue} at {ms} ms over {lane}: a feed-reported \
                     disconnect lasting longer than that cancels this venue's resting orders \
                     (action: {})",
                    policy.deadman_action.as_str()
                ),
                (Some(ms), true, _, AdapterLinkDisclosure::StratLinkOnly { lane }) => format!(
                    "LINK DEAD-MAN ARMED for {venue} at {ms} ms over {lane}: a datahub OR gatewayhub \
                     outage lasting longer than that cancels this venue's resting orders (action: {}). \
                     The venue's own socket can never trip it; the link to the datahub can, and so can \
                     the account's trade link to the gatewayhub (the switch arms per venue, not per \
                     source), so a gatewayhub restart longer than that halts trading on {venue} too",
                    policy.deadman_action.as_str()
                ),
                (Some(_), false, Some(why), _) => format!(
                    "LINK DEAD-MAN off for {venue}: {why}. This venue has NO automatic stop for a \
                     dead link — and that includes the strategy-lane link to the datahub, which \
                     goes dark with it: the switch arms per VENUE, not per source, so arming it for \
                     that link would arm the venue's own socket with it. \
                     `vike_venues::link_deadman_default` is the row, and it says what would change it"
                ),
                (Some(_), false, None, AdapterLinkDisclosure::Discloses { lane })
                | (Some(_), false, None, AdapterLinkDisclosure::StratLinkOnly { lane }) => format!(
                    "LINK DEAD-MAN off for {venue}: the venue's bridge DOES report a dead link, \
                     but THIS DAEMON subscribes no lane that carries it — {lane}. So \
                     there is no automatic stop for a dead {venue} link here, whatever the venue \
                     table says"
                ),
            }
        })
        .collect()
}

/// **The absent-key warning** — the message the live start emits ONCE when there is no
/// `policy.deadman_timeout_ms` row, so that a switch which is off by omission is at least off in
/// the log. `None` for `Some(0)` (the operator decided, and wrote it) and for `Some(n)` (armed).
///
/// # Why a WARNING and not a default, and why not silence either
///
/// The switch this key arms observes SILENCE across the whole core's ingest, not the connection
/// (`vike_config::Policy::deadman_timeout_ms` carries the derivation and the morning's reversed
/// ruling), so a compiled-in default halts every session-bounded venue at every close — which is
/// why there is none. But a key nobody has heard of is not a decision: a 24/7 crypto start that
/// WOULD want the switch gets the same silence as an FX start that would be harmed by it, and the
/// daemon cannot tell them apart. So the start says, once, that the switch is off, what it would
/// do, and the one line that arms it — and `deadman_timeout_ms = 0` is the spelling that says
/// "I read this and decided", which is the ONLY thing that silences it. The shape is
/// `crates/vike-tradehub-core/src/arming.rs`'s `unaddressable_accounts_message`: name the key, name what
/// was refused (here: nothing is armed), and end with a paste-ready block
/// the operator CHOOSES rather than copies — the recommendation is
/// `vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS` and it is wrong for a venue that closes, so the
/// text says so beside the number.
///
/// Returns the message rather than logging it, so the decision is testable with no subscriber
/// (the `unaddressable_accounts_message` idiom); [`warn_deadman_absent`] is the `Once` latch over
/// it.
///
/// ⚠ **The headline read `DEAD-MAN SWITCH IS OFF` and the body read "this live start has NO
/// automatic stop".** Both became FALSE the day the CONNECTION-state dead-man shipped ARMED BY
/// DEFAULT (M13 — [`link_deadman_config_from_policy`] above): an operator reading the old text
/// would conclude they were unprotected, and would then either write a key that halts their FX
/// start every Friday or trust a warning that no longer described their daemon. The message names
/// WHICH switch is off, says the other one is on, and points at the per-venue LINK DEAD-MAN lines
/// emitted beside it. `crates/vike-tradehub/tests/deadman_absent_warning.rs` asserts the retired
/// sentence cannot come back.
///
/// ⚠ **"The other one is on" is BRANCHED on, not asserted.** This function holds the very
/// [`vike_config::Policy`] that may carry `link_deadman_grace_ms = 0`, and the first rewrite
/// stated the sibling was armed unconditionally — telling the one operator who had turned BOTH
/// switches off that they had a protection they had explicitly removed. On that box the message
/// says so instead, and names the line that did it.
pub fn deadman_absent_warning(policy: &vike_config::Policy) -> Option<String> {
    if policy.deadman_timeout_ms.is_some() {
        return None;
    }
    // ⚠ **THE REMEDY NAMES THE ONE STORE THERE IS** — rendered by `vike_config::remedy`, never
    // spelled here, so it cannot send an operator to write somewhere nothing reads
    // (`docs/decisions/0086`: one store, one remedy, on every box).
    let remedy = vike_config::SettingsSection::Policy.write_remedy("deadman_timeout_ms");
    // ⚠ BRANCH on the OTHER switch's effective state rather than asserting it. This function holds
    // the very `Policy` that may say `link_deadman_grace_ms = 0`, and on such a box the
    // unconditional sentence told the operator a switch was armed while every per-venue line below
    // said it was off for that exact reason.
    let sibling: String = match policy.link_deadman_grace_ms_effective() {
        Some(_) => "\
         ⚠ Do not read that as \"no automatic stop\": the CONNECTION-state dead-man is armed by \
         default and is a DIFFERENT key. It cancels a venue's resting orders when that venue's \
         own feed reports the link DISCONNECTED for longer than `link_deadman_grace_ms`, and it \
         stays quiet through a market that merely closed — which is why it may have an armed \
         default and this key may not.\n\n\
         ⚠ Armed by default is not armed EVERYWHERE, and this message will not guess for you: \
         where it arms is a per-venue question (every FX and equities venue is off because its \
         market has sessions, several venues disclose no disconnect at all, and a venue whose \
         only emitter is a lane this daemon does not subscribe is off too). The per-venue LINK \
         DEAD-MAN lines beside this message are the answer for THIS mount — read those, not this \
         paragraph. This key is the optional EXTRA for a 24/7 mount that also wants a silence \
         detector."
            .to_string(),
        None => {
            // ⚠ The SIBLING's way back is the same class as this message's own remedy, rendered
            // through the same `vike_config::remedy` so the two halves of one warning can never
            // disagree about how to write it.
            let grace = vike_config::SettingsSection::Policy.write_remedy("link_deadman_grace_ms");
            format!(
                "\
         ⚠ AND NEITHER IS THE OTHER ONE: you wrote `link_deadman_grace_ms = 0`, which turns the \
         CONNECTION-state dead-man off for EVERY venue. So this mount has NO automatic stop of \
         any kind — not for a dead socket and not for a quiet feed. {} to restore the \
         connection-state switch at its default grace; this key stays a separate decision.",
                grace.unset_clause()
            )
        }
    };
    Some(format!(
        "THE SILENCE DEAD-MAN IS OFF: {}, so this live mount has no automatic stop for \"the feed \
         went QUIET and orders are still resting\". It is off by OMISSION, not by decision — \
         nothing arms it unless you write the key, and this line is the only place that says \
         so.\n\n\
         {sibling}\n\n\
         What THIS key would add: after that many milliseconds with no venue event, tick, bar, \
         quote, trade or book update on ANY venue or symbol this daemon follows, cancel every \
         resting order and (with the default `deadman_action`) engage HALT — `Halted` on every \
         engine plus the HALT sentinel file, which an operator must delete before the next \
         submit.\n\n\
         ⚠ It observes SILENCE, not the connection. On a venue whose market CLOSES (FX over the \
         weekend, equities every evening) or on a thin instrument in a quiet minute it trips with \
         no outage at all. Arm it on a 24/7 mount only.\n\n\
         To arm it for a venue that never closes, {} \
         (milliseconds; {} is the recommendation for a 24/7 venue, raise it for a thin one):\n\n\
         {}\n\n\
         To record that you decided AGAINST it, and silence this message:\n\n\
         {}\n",
        remedy.absent_clause(),
        remedy.write_clause(),
        vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS,
        remedy.write_line(&vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS.to_string()),
        remedy.write_line("0"),
    ))
}
