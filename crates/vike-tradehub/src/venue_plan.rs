//! Per-venue feed-plan construction: prove a mount's `(symbol, mainnet)` pair is wired
//! (`build_node` mounts an engine, the symbol has a usable price grid) before yielding a
//! `VenuePlan`. Moved out of `main.rs` so this crate's integration tests can drive it directly —
//! see this crate's `CLAUDE.md` on why untested logic living only in `main.rs` is the defect.

use std::collections::HashMap;

use vike_run::MakerMountConfig;

use crate::{CexVenue, VenuePlan};

/// The symbol `vike_run::build_node` mounts `venue`'s `ExecutionEngine` on, read from the ONE table
/// that decides it ([`vike_run::WIRED_MARKETS`]) rather than restated as a literal here.
///
/// `None` means `build_node` mounts no engine for that venue at all — orders would have nowhere to
/// go. Kept separate from [`vike_tradehub::config::DaemonProfile::validate_for_live`]'s identical
/// lookup ON PURPOSE: that one gates the PROFILE path, this one gates `live_mount` itself, so the
/// refusal exists whether or not a profile was validated (an operator can reach `live_mount`
/// through the `MakerMountConfig` defaults without a `[live]` profile ever being checked).
pub fn wired_symbol_for(venue: &str) -> Option<&'static str> {
    vike_run::WIRED_MARKETS.iter().find(|(v, _)| *v == venue).map(|&(_, s)| s)
}

/// The allow-list gate for a CEX venue: prove `cfg` names the symbol this venue's engine accepts and
/// carries a usable price grid, then yield the plan. Shared by the binance/bybit/okx arms — one
/// implementation, so the three cannot drift on which refusals they perform.
///
/// TWO refusals, both of which are otherwise SILENT failures:
///
/// 1. **The symbol.** `vike_mount::make_engine` sets no `extra_symbols`, so
///    [`vike_exec::ExecutionEngine::accepts_symbol`] is a plain equality test: a foreign symbol's
///    orders AND fills are dropped with no log, the strategy quotes into the void, and nothing ever
///    trades. Refuse instead.
/// 2. **The tick size.** `cfg.tick_size` sizes the `L2Book` PRICE GRID handed to
///    `spawn_*_market_data`, and it is a plain `f64` parameter with no validation anywhere below —
///    pass `0.0` (or a NaN from a malformed profile) and the book is built on a degenerate grid that
///    reports no error, produces no usable top-of-book, and leaves the maker unable to quote. It is
///    also the maker's OWN quoting grid (`MakerMountConfig::crypto`), so the two are the same number
///    by construction and this one check covers both.
pub fn cex_plan(
    venue: CexVenue,
    cfg: &MakerMountConfig,
    mainnet: bool,
) -> Result<VenuePlan, String> {
    let slug = venue.slug();
    let Some(wired) = wired_symbol_for(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `build_node` mounts no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by build_node, but this mount names {:?}: that engine \
             accepts exactly its mounted symbol (`ExecutionEngine::accepts_symbol` — no \
             `extra_symbols` are wired), so every order and fill would be SILENTLY DROPPED",
            cfg.token_id
        ));
    }
    if !(cfg.tick_size.is_finite() && cfg.tick_size > 0.0) {
        return Err(format!(
            "{slug} mount needs a POSITIVE finite tick_size (it sizes both the L2Book price grid \
             the quote pump folds into and the maker's own quoting grid), got {}",
            cfg.tick_size
        ));
    }
    Ok(VenuePlan::Cex { venue, mainnet })
}

/// The allow-list gate for ALPACA (split-plane I9) — the first CREDENTIALED-DATA venue: same
/// symbol + tick-size refusals as [`cex_plan`], plus TWO alpaca-shaped ones, each converting a
/// silent do-nothing into a startup error:
///
/// 1. **The interval.** `AlpacaDataClient::subscribe_bars` serves the market-data WS's fixed
///    1-minute bars ONLY and returns `Unsupported` for anything else — and bars are a SAFETY
///    dependency on a live mount (the margin-call watchdog and the drawdown latch sweep per
///    CLOSED bar), so a `5m` profile would mount, subscribe nothing on the bar lane, and run with
///    both watchdogs dead. Refused here, before the core spawns.
/// 2. **The credentials.** The data WS AUTHENTICATES (OAuth Bearer from the SANDBOX
///    client-credentials trio) — there is no keyless stream to fall back to, so "absent
///    credentials ⇒ paper" cannot supply prices the way it does for every CEX venue. A live mount
///    without a feed quotes into the void forever, which is the defect class this repo names
///    silent-do-nothing; the gate REFUSES instead, naming the exact keys. This is a DOCUMENTED
///    divergence from the exec gate (which degrades to paper): the exec fallback leaves a working
///    mount, the feed fallback would leave a dead one.
///
/// Resolution goes through `vike_alpaca::load_alpaca_config_from(Environment::Demo, vars)` — the
/// SAME loader, tier and vars map `vike_mount::make_engine`'s `("alpaca", _)` exec arm reads
/// (`Demo` ⇒ the `SANDBOX` key tier + sandbox hosts, `alpaca_tier`). Never a second env read: the
/// map is the daemon's one `workspace_credentials()` sweep.
pub fn alpaca_plan(
    cfg: &MakerMountConfig,
    vars: &HashMap<String, String>,
) -> Result<VenuePlan, String> {
    let slug = "alpaca";
    let Some(wired) = wired_symbol_for(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `build_node` mounts no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by build_node, but this mount names {:?}: that engine \
             accepts exactly its mounted symbol (`ExecutionEngine::accepts_symbol` — no \
             `extra_symbols` are wired), so every order and fill would be SILENTLY DROPPED",
            cfg.token_id
        ));
    }
    if !(cfg.tick_size.is_finite() && cfg.tick_size > 0.0) {
        return Err(format!(
            "{slug} mount needs a POSITIVE finite tick_size (it is the maker's own quoting grid), \
             got {}",
            cfg.tick_size
        ));
    }
    if cfg.interval != "1m" {
        return Err(format!(
            "{slug}'s market-data WS serves 1m bars ONLY (`AlpacaDataClient::subscribe_bars`), \
             and bars drive the live watchdogs — set interval = \"1m\", got {:?}",
            cfg.interval
        ));
    }
    let upper = slug.to_uppercase();
    let tier = vike_alpaca::alpaca_tier(vike_bridge_core::Environment::Demo);
    match vike_alpaca::load_alpaca_config_from(vike_bridge_core::Environment::Demo, vars) {
        Some(config) => Ok(VenuePlan::Alpaca(Box::new(config))),
        None => Err(format!(
            "{slug}'s market data is CREDENTIALED (the WS auths with the OAuth2 {tier} trio), so \
             a live {slug} mount cannot run on absent credentials the way a keyless-feed venue \
             mounts paper — add {upper}_{tier}_CLIENT_ID + {upper}_{tier}_CLIENT_SECRET + \
             {upper}_{tier}_ACCOUNT_ID to <project>/settings/secrets.env (the same trio arms \
             {tier} exec), or drop the live gate for a paper daemon"
        )),
    }
}

/// The allow-list gate for CTRADER (split-plane I9) — the second credentialed-data venue: the
/// [`cex_plan`] symbol + tick-size refusals, plus the credential refusal (same argument as
/// [`alpaca_plan`]'s: no keyless stream exists, and a feed-less live mount is a silent
/// do-nothing).
///
/// No interval refusal here, deliberately: the ctrader feed arm does NOT subscribe trendbars —
/// `conn::on_inbound`'s live trendbar lane only ever emits `forming_bar` (cTrader pushes forming
/// snapshots; no close fires through this client), so real venue bars cannot drive the per-CLOSED-
/// bar watchdogs at all. The arm synthesizes closed bars from quote mids through
/// [`vike_run::MakerSink`] instead (the polymarket shape), whose window is `cfg.interval_ms` —
/// any positive window works, and [`check_ctrader_intervals`] holds the one cross-mount
/// constraint that shape creates.
///
/// Resolution goes through `CtraderConfig::from_vars(Environment::Demo, vars)` — the SAME loader,
/// tier and vars map `vike_mount::make_engine`'s `("ctrader", _)` exec arm reads (`Demo` ⇒ the
/// `CTRADER_DEMO_*` token tier + `demo.ctraderapi.com`).
pub fn ctrader_plan(
    cfg: &MakerMountConfig,
    vars: &HashMap<String, String>,
) -> Result<VenuePlan, String> {
    let slug = "ctrader";
    let Some(wired) = wired_symbol_for(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `build_node` mounts no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by build_node, but this mount names {:?}: that engine \
             accepts exactly its mounted symbol (`ExecutionEngine::accepts_symbol` — no \
             `extra_symbols` are wired), so every order and fill would be SILENTLY DROPPED",
            cfg.token_id
        ));
    }
    if !(cfg.tick_size.is_finite() && cfg.tick_size > 0.0) {
        return Err(format!(
            "{slug} mount needs a POSITIVE finite tick_size (it is the maker's own quoting grid), \
             got {}",
            cfg.tick_size
        ));
    }
    let upper = slug.to_uppercase();
    let tier = "DEMO";
    match vike_ctrader::config::CtraderConfig::from_vars(vike_bridge_core::Environment::Demo, vars)
    {
        Some(config) => Ok(VenuePlan::Ctrader(Box::new(config))),
        None => Err(format!(
            "{slug}'s market data is CREDENTIALED (the protobuf socket runs the OAuth two-stage \
             auth), so a live {slug} mount cannot run on absent credentials the way a keyless-feed \
             venue mounts paper — add {upper}_CLIENT_ID + {upper}_CLIENT_SECRET (the Spotware app \
             registration) plus {upper}_{tier}_ACCESS_TOKEN + {upper}_{tier}_REFRESH_TOKEN (and \
             optionally {upper}_{tier}_ACCOUNT_ID) to <project>/settings/secrets.env (the same \
             set arms {tier} exec), or drop the live gate for a paper daemon"
        )),
    }
}

/// The allow-list gate for OANDA (split-plane I9) — the third credentialed-data venue, and the
/// only live-wired one whose feed rides no socket at all: the [`cex_plan`] symbol + tick-size
/// refusals, the credential refusal (same argument as [`alpaca_plan`]'s — the pricing stream and
/// the candles endpoint are BOTH Bearer-authed, there is no keyless oanda lane, and a feed-less
/// live mount is a silent do-nothing), plus one oanda-shaped interval refusal.
///
/// **The interval refusal, and why it is not alpaca's.** Alpaca is pinned to `1m` because its WS
/// serves exactly one bar width. OANDA serves MANY: `subscribe_bars` polls the candles endpoint,
/// which is keyed by a venue GRANULARITY code, and `vike_oanda::granularity` is the total function
/// from a vike interval string to that code. An interval it cannot map has no candle series on
/// this venue, so the subscription would be refused inside the feed — after the core had spawned,
/// leaving a live mount whose bar lane is dead and, with it, the margin-call watchdog and the 25%
/// drawdown latch, both of which sweep per CLOSED bar and nowhere else. Refused here instead,
/// before a thread exists. The check is DERIVED from that same table rather than restating a list
/// of intervals, so the two cannot drift.
///
/// ⚠ Deliberately NO same-venue interval gate ([`check_ctrader_intervals`]'s shape does not
/// transfer): oanda's bars are REAL venue candles on the lossless `close_bar` lane through
/// `CoreLaneSink`, not one shared `MakerSink` synthesizer, so two oanda mounts at different
/// intervals subscribe two independent poll threads and both bar lanes fire. The ctrader gate
/// exists because a synthesizer has exactly one window; this venue has none.
///
/// Resolution goes through `load_oanda_config_from(Environment::Demo, vars)` — the SAME loader,
/// tier and vars map `vike_mount::make_engine`'s `("oanda", _)` exec arm reads (`Demo` ⇒ the
/// `OANDA_DEMO_*` pair + the fxPractice REST/stream hosts, `vike_oanda::oanda_hosts`). Never a
/// second env read.
pub fn oanda_plan(
    cfg: &MakerMountConfig,
    vars: &HashMap<String, String>,
) -> Result<VenuePlan, String> {
    let slug = "oanda";
    let Some(wired) = wired_symbol_for(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `build_node` mounts no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by build_node, but this mount names {:?}: that engine \
             accepts exactly its mounted symbol (`ExecutionEngine::accepts_symbol` — no \
             `extra_symbols` are wired), so every order and fill would be SILENTLY DROPPED",
            cfg.token_id
        ));
    }
    if !(cfg.tick_size.is_finite() && cfg.tick_size > 0.0) {
        return Err(format!(
            "{slug} mount needs a POSITIVE finite tick_size (it is the maker's own quoting grid), \
             got {}",
            cfg.tick_size
        ));
    }
    if vike_oanda::granularity(&cfg.interval).is_none() {
        return Err(format!(
            "{slug} serves candles by GRANULARITY code and has none for interval {:?} \
             (`vike_oanda::granularity` is the table), so the bar lane would subscribe nothing — \
             and closed bars are what fire the margin-call watchdog and the drawdown latch. Name \
             an interval this venue serves",
            cfg.interval
        ));
    }
    let upper = slug.to_uppercase();
    let tier = vike_bridge_core::Environment::Demo.as_str();
    match vike_oanda::load_oanda_config_from(vike_bridge_core::Environment::Demo, vars) {
        Some(config) => Ok(VenuePlan::Oanda(Box::new(config))),
        None => Err(format!(
            "{slug}'s market data is CREDENTIALED on BOTH lanes (the pricing stream and the \
             candles endpoint are Bearer-authed against one account), so a live {slug} mount \
             cannot run on absent credentials the way a keyless-feed venue mounts paper — add \
             {upper}_{tier}_API_KEY + {upper}_{tier}_ACCOUNT_ID to \
             <project>/settings/secrets.env (the same pair arms {tier} exec), or drop the live \
             gate for a paper daemon"
        )),
    }
}

/// The allow-list gate for DERIBIT (split-plane I9) — the [`cex_plan`] symbol + tick-size
/// refusals plus one deribit-shaped interval refusal, and **deliberately NO credential refusal**.
///
/// **Why there is no credential gate here, when the three arms above all have one.** Every lane
/// this venue's feed serves is a KEYLESS public read on `www.deribit.com`
/// (`crates/bridges/deribit/src/market_feed.rs` dials `crate::options_feed::MAINNET_WS` and sends
/// no auth frame; the REST warmup seed goes through `crate::data`, likewise keyless). So absent
/// credentials leave a deribit mount with a fully working feed and a PAPER exec book — the
/// workspace-wide live gate, byte-identical to a CEX mount — and refusing startup would invent a
/// failure the venue does not have. The alpaca/ctrader/oanda refusals exist because THEIR feeds
/// authenticate and a feed-less live mount quotes into the void; that argument simply does not
/// reach this venue.
///
/// **The interval refusal, and why it is oanda's shape rather than alpaca's.** Deribit's bar lane
/// subscribes `chart.trades.{inst}.{res}`, where `{res}` is a venue RESOLUTION code, and
/// `vike_deribit::data::resolution_code` is the total function from a vike interval string to it.
/// The enum has GAPS (there is no 4h, no weekly, no monthly), and an unmappable interval is a
/// PERMANENT config error the feed thread answers by setting a status line and RETURNING — no
/// reconnect fixes it, so the mount would run with a dead bar lane and, with it, the margin-call
/// watchdog and the 25% drawdown latch, both of which sweep per CLOSED bar and nowhere else.
/// Refused here instead, before a thread exists. DERIVED from that same table in both directions
/// by its unit test, never restated as a list of intervals.
///
/// ⚠ Deliberately NO same-venue interval gate ([`check_ctrader_intervals`]'s shape does not
/// transfer): deribit's bars are REAL venue candles on the lossless `close_bar` lane through
/// `CoreLaneSink`, so two deribit mounts at different intervals subscribe two independent chart
/// sockets and both bar lanes fire. The ctrader gate exists because a synthesizer has exactly one
/// window; this venue has none.
pub fn deribit_plan(cfg: &MakerMountConfig) -> Result<VenuePlan, String> {
    let slug = "deribit";
    let Some(wired) = wired_symbol_for(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `build_node` mounts no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by build_node, but this mount names {:?}: that engine \
             accepts exactly its mounted symbol (`ExecutionEngine::accepts_symbol` — no \
             `extra_symbols` are wired), so every order and fill would be SILENTLY DROPPED",
            cfg.token_id
        ));
    }
    if !(cfg.tick_size.is_finite() && cfg.tick_size > 0.0) {
        return Err(format!(
            "{slug} mount needs a POSITIVE finite tick_size (it is the maker's own quoting grid), \
             got {}",
            cfg.tick_size
        ));
    }
    if let Err(e) = vike_deribit::data::resolution_code(&cfg.interval) {
        return Err(format!(
            "{slug} streams `chart.trades` by RESOLUTION code and has none for interval {:?} \
             ({e} — `vike_deribit::data::resolution_code` is the table, and its enum has gaps: \
             there is no 4h), so the bar lane's thread would set a status line and exit — and \
             closed bars are what fire the margin-call watchdog and the drawdown latch. Name an \
             interval this venue serves",
            cfg.interval
        ));
    }
    Ok(VenuePlan::Deribit)
}

/// The allow-list gate for IG (split-plane I9) — the fourth credentialed-data venue: the
/// [`cex_plan`] symbol + tick-size refusals, the credential refusal ([`alpaca_plan`]'s argument —
/// every Lightstreamer subscription opens its own `IgSession` login, there is no keyless IG lane
/// at all, and a feed-less live mount is a silent do-nothing), plus one IG-shaped interval
/// refusal.
///
/// **The interval refusal, and why it is oanda's shape rather than alpaca's.** IG's bar lane
/// subscribes `CHART:{epic}:{scale}`, where `{scale}` is a Lightstreamer chart scale, and
/// `vike_ig::market_data::ig_scale` is the total function from a vike interval string to it. That
/// STREAMING set is deliberately narrower than the crate's REST `resolution` ladder — the venue
/// streams only a few widths — so an interval it cannot map has no live candle item on this venue.
/// `subscribe_bars` would refuse it, but only INSIDE the feed, after the core had spawned, leaving
/// a live mount whose bar lane is dead and with it the margin-call watchdog and the 25% drawdown
/// latch, both of which sweep per CLOSED bar and nowhere else. Refused here instead, before a
/// thread exists. DERIVED from that same table in both directions by its unit test, never restated
/// as a list of intervals.
///
/// ⚠ Deliberately NO same-venue interval gate ([`check_ctrader_intervals`]'s shape does not
/// transfer): IG's bars are REAL venue candles on the lossless `close_bar` lane through
/// `CoreLaneSink` (`CandleFold` closes a bar on `CONS_END` or a `UTM` advance, whichever arrives
/// first), so two IG mounts at different intervals subscribe two independent TLCP sessions and
/// both bar lanes fire. The ctrader gate exists because a synthesizer has exactly one window; this
/// venue has none.
///
/// Resolution goes through `load_ig_config_from(Environment::Demo, vars)` — the SAME loader, tier
/// and vars map `vike_mount::make_engine`'s `("ig", _)` exec arm reads (`Demo` ⇒ the `IG_DEMO_*`
/// trio + the demo dealing gateway, `vike_ig::ig_rest_base`). Never a second env read.
pub fn ig_plan(
    cfg: &MakerMountConfig,
    vars: &HashMap<String, String>,
) -> Result<VenuePlan, String> {
    let slug = "ig";
    let Some(wired) = wired_symbol_for(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `build_node` mounts no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by build_node, but this mount names {:?}: that engine \
             accepts exactly its mounted symbol (`ExecutionEngine::accepts_symbol` — no \
             `extra_symbols` are wired), so every order and fill would be SILENTLY DROPPED",
            cfg.token_id
        ));
    }
    if !(cfg.tick_size.is_finite() && cfg.tick_size > 0.0) {
        return Err(format!(
            "{slug} mount needs a POSITIVE finite tick_size (it is the maker's own quoting grid), \
             got {}",
            cfg.tick_size
        ));
    }
    if vike_ig::market_data::ig_scale(&cfg.interval).is_none() {
        return Err(format!(
            "{slug} streams candles by Lightstreamer SCALE and has none for interval {:?} \
             (`vike_ig::market_data::ig_scale` is the table, and it is deliberately narrower than \
             this venue's REST history ladder), so the bar lane would subscribe nothing — and \
             closed bars are what fire the margin-call watchdog and the drawdown latch. Name an \
             interval this venue streams",
            cfg.interval
        ));
    }
    let upper = slug.to_uppercase();
    let tier = vike_bridge_core::Environment::Demo.as_str();
    match vike_ig::load_ig_config_from(vike_bridge_core::Environment::Demo, vars) {
        Some(config) => Ok(VenuePlan::Ig(Box::new(config))),
        None => Err(format!(
            "{slug}'s market data is CREDENTIALED (every Lightstreamer subscription opens its own \
             {slug} REST login for streaming credentials), so a live {slug} mount cannot run on \
             absent credentials the way a keyless-feed venue mounts paper — add \
             {upper}_{tier}_API_KEY + {upper}_{tier}_IDENTIFIER + {upper}_{tier}_PASSWORD to \
             <project>/settings/secrets.env (the same trio arms {tier} exec), or drop the live \
             gate for a paper daemon"
        )),
    }
}

/// The ctrader same-venue interval gate (split-plane I9) — the ctrader spelling of
/// `check_poly_token_intervals`'s (in `main.rs`) constraint: every ctrader mount shares ONE wired symbol
/// (`EURUSD` — [`ctrader_plan`] refused anything else per row), so all of them share one
/// [`vike_run::MakerSink`] whose `TickBarSynthesizer` runs at exactly ONE window. Rows naming
/// different intervals would leave every mount after the first with a silently dead bar lane —
/// no strategy bars, no watchdog cadence. Refused by ROW, before any core spawns.
///
/// Pure over the lowered mount configs so the refusal is unit-testable without a `ResolvedMount`
/// (which carries a constructed strategy).
pub fn check_ctrader_intervals(cfgs: &[&MakerMountConfig]) -> Result<(), String> {
    for (j, m) in cfgs.iter().enumerate() {
        if m.venue != "ctrader" {
            continue;
        }
        let earlier = cfgs[..j].iter().enumerate().find(|(_, p)| p.venue == "ctrader");
        if let Some((i, first)) = earlier
            && (first.interval != m.interval || first.interval_ms != m.interval_ms)
        {
            return Err(format!(
                "mounts[{i}] and mounts[{j}] both trade ctrader {} but at different intervals \
                     ({}/{} ms vs {}/{} ms): ctrader's one data socket feeds ONE bar synthesizer, \
                     and its synthesized closed bars run at exactly one window — the second \
                     interval would silently never fire. Give both rows one interval, or split \
                     the venues",
                m.token_id, first.interval, first.interval_ms, m.interval, m.interval_ms,
            ));
        }
    }
    Ok(())
}
