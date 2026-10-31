//! Per-venue feed-plan construction: prove a mount's `(symbol, mainnet)` pair is wired
//! (`assemble_runtime` mounts an engine, the symbol has a usable price grid) before yielding a
//! `VenuePlan`. Moved out of `main.rs` so this crate's integration tests can drive it directly —
//! see this crate's `CLAUDE.md` on why untested logic living only in `main.rs` is the defect.

use vike_tradehub_core::MakerConfig;
use vike_venues::venue_wiring::wired_symbol;

use crate::{CexVenue, VenuePlan};

/// The allow-list gate for a CEX venue: prove `cfg` names the symbol this venue's engine accepts and
/// carries a usable price grid, then yield the plan. Shared by the binance/bybit/okx arms — one
/// implementation, so the three cannot drift on which refusals they perform.
///
/// TWO refusals, both of which are otherwise SILENT failures:
///
/// 1. **The symbol.** `vike_tradehub_core::make_engine` sets no `extra_symbols`, so
///    [`vike_exec::ExecutionEngine::accepts_symbol`] is a plain equality test: a foreign symbol's
///    orders AND fills are dropped with no log, the strategy quotes into the void, and nothing ever
///    trades. Refuse instead.
/// 2. **The tick size.** `cfg.tick_size` sizes the `L2Book` PRICE GRID handed to
///    `spawn_*_market_data`, and it is a plain `f64` parameter with no validation anywhere below —
///    pass `0.0` (or a NaN from a malformed profile) and the book is built on a degenerate grid that
///    reports no error, produces no usable top-of-book, and leaves the maker unable to quote. It is
///    also the maker's OWN quoting grid (`MakerConfig::crypto`), so the two are the same number
///    by construction and this one check covers both.
pub fn cex_plan(venue: CexVenue, cfg: &MakerConfig, mainnet: bool) -> Result<VenuePlan, String> {
    let slug = venue.slug();
    let Some(wired) = wired_symbol(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `assemble_runtime` builds no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by assemble_runtime, but this mount names {:?}: that engine \
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
/// symbol + tick-size refusals as [`cex_plan`], plus one alpaca-shaped one that converts a silent
/// do-nothing into a startup error:
///
/// **The interval.** `AlpacaDataClient::subscribe_bars` serves the market-data WS's fixed
/// 1-minute bars ONLY and returns `Unsupported` for anything else — and bars are a SAFETY
/// dependency on a live mount (the margin-call watchdog and the drawdown latch sweep per
/// CLOSED bar), so a `5m` profile would mount, subscribe nothing on the bar lane, and run with
/// both watchdogs dead. Refused here, before the core spawns.
///
/// **The credentials are NOT resolved here any more** (wave 2): the data WS AUTHENTICATES (OAuth Bearer
/// from the sandbox client-credentials trio), so there is no keyless stream to fall back to, and a live
/// mount without a feed quotes into the void forever. The feed is a credentialed stream at the
/// gatewayhub, which reads the venue's DEFAULT `demo` account itself
/// (`AccountDirectory::data_feed_credentials`) and REFUSES the open naming the missing fields; that
/// refusal surfaces when the arm subscribes (`crate::feeds`' alpaca arm) and refuses the start there,
/// as this gate did.
pub fn alpaca_plan(cfg: &MakerConfig) -> Result<VenuePlan, String> {
    let slug = "alpaca";
    let Some(wired) = wired_symbol(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `assemble_runtime` builds no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by assemble_runtime, but this mount names {:?}: that engine \
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
    Ok(VenuePlan::Alpaca)
}

/// The allow-list gate for CTRADER (split-plane I9) — the second credentialed-data venue: the
/// [`cex_plan`] symbol + tick-size refusals. The credential refusal is the gatewayhub's now (see
/// [`alpaca_plan`]): ctrader's handshake is SYNCHRONOUS at the gatewayhub's open, so a missing
/// token pair, a bad grant or an unreachable host refuses the `DataOpen` and the arm refuses the
/// start, exactly as `connect_and_auth` failing here did.
///
/// No interval refusal here, deliberately: the ctrader feed arm does NOT subscribe trendbars —
/// `conn::on_inbound`'s live trendbar lane only ever emits `forming_bar` (cTrader pushes forming
/// snapshots; no close fires through this client), so real venue bars cannot drive the per-CLOSED-
/// bar watchdogs at all. The arm synthesizes closed bars from quote mids through
/// [`vike_tradehub_core::MakerSink`] instead (the polymarket shape), whose window is `cfg.interval_ms` —
/// any positive window works, and [`check_ctrader_intervals`] holds the one cross-mount
/// constraint that shape creates.
pub fn ctrader_plan(cfg: &MakerConfig) -> Result<VenuePlan, String> {
    let slug = "ctrader";
    let Some(wired) = wired_symbol(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `assemble_runtime` builds no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by assemble_runtime, but this mount names {:?}: that engine \
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
    Ok(VenuePlan::Ctrader)
}

/// The allow-list gate for OANDA (split-plane I9) — the third credentialed-data venue, whose feed
/// rides no socket of its own: the [`cex_plan`] symbol + tick-size refusals. The credential refusal
/// is the gatewayhub's (see [`alpaca_plan`]).
///
/// ⚠ **The interval refusal LEFT this gate.** It used to ask the bridge's granularity table (a venue
/// GRANULARITY code per interval) so an unmappable interval was refused BEFORE the core spawned; the
/// table is the bridge's, and this crate links no bridge. The bridge refuses the same interval at
/// `subscribe_bars` (inside the gatewayhub), the arm turns the refusal into the start's refusal, and
/// closed bars — what fires the margin-call watchdog and the drawdown latch — are never left to a
/// dead lane. The outcome is the same refusal; it arrives from the subscription, after the core spawned,
/// with the bridge's words.
///
/// ⚠ Deliberately NO same-venue interval gate ([`check_ctrader_intervals`]'s shape does not
/// transfer): oanda's bars are REAL venue candles on the lossless `close_bar` lane through
/// `CoreLaneSink`, not one shared `MakerSink` synthesizer, so two oanda mounts at different
/// intervals subscribe two independent poll threads and both bar lanes fire. The ctrader gate
/// exists because a synthesizer has exactly one window; this venue has none.
pub fn oanda_plan(cfg: &MakerConfig) -> Result<VenuePlan, String> {
    let slug = "oanda";
    let Some(wired) = wired_symbol(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `assemble_runtime` builds no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by assemble_runtime, but this mount names {:?}: that engine \
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
    Ok(VenuePlan::Oanda)
}

/// The allow-list gate for DERIBIT (split-plane I9) — the [`cex_plan`] symbol + tick-size
/// refusals, and **deliberately NO credential refusal**.
///
/// **Why there is no credential gate here, when the four credentialed arms have one.** Every lane
/// this venue's feed serves is a KEYLESS public read on `www.deribit.com`, so absent credentials
/// leave a deribit mount with a fully working feed and a PAPER exec book — the workspace-wide live
/// gate, byte-identical to a CEX mount — and refusing startup would invent a failure the venue does
/// not have.
///
/// ⚠ **The interval refusal LEFT this gate** (it asked the bridge's resolution table, whose enum has
/// gaps — there is no 4h). This crate links no bridge now, and the refusal is the GATEWAYHUB's: its deribit
/// client is built behind `BarIntervalGate` (`crates/vike-gatewayhub/src/data/venues.rs`'s
/// `deribit_interval`, this gate's old text verbatim), so a `subscribe_bars` for an interval outside
/// `1m 3m 5m 10m 15m 30m 1h 2h 3h 6h 12h 1d` is refused there, before the inner client is asked, and the
/// refusal reaches the operator as the start's refusal (`tests/refusal_chain.rs` sends it through the real
/// hops). It is no longer the dead bar lane, and the margin-call watchdog and drawdown latch keep their cadence.
///
/// ⚠ Deliberately NO same-venue interval gate ([`check_ctrader_intervals`]'s shape does not
/// transfer): deribit's bars are REAL venue candles on the lossless `close_bar` lane through
/// `CoreLaneSink`, so two deribit mounts at different intervals subscribe two independent chart
/// sockets and both bar lanes fire. The ctrader gate exists because a synthesizer has exactly one
/// window; this venue has none.
pub fn deribit_plan(cfg: &MakerConfig) -> Result<VenuePlan, String> {
    let slug = "deribit";
    let Some(wired) = wired_symbol(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `assemble_runtime` builds no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by assemble_runtime, but this mount names {:?}: that engine \
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
    Ok(VenuePlan::Deribit)
}

/// The allow-list gate for IG (split-plane I9) — the fourth credentialed-data venue: the
/// [`cex_plan`] symbol + tick-size refusals. The credential refusal is the gatewayhub's (see
/// [`alpaca_plan`]; the IG client logs in with an `IgSession` for the streaming credentials its
/// Lightstreamer subscriptions share, so there is no keyless IG lane at all).
///
/// ⚠ **The interval refusal LEFT this gate** (it asked the bridge's STREAMING chart scales, deliberately
/// narrower than the REST history ladder). This crate links no bridge now; the bridge refuses the
/// interval at `subscribe_bars` inside the gatewayhub and the arm turns that into the start's refusal
/// (see [`oanda_plan`]).
///
/// ⚠ Deliberately NO same-venue interval gate ([`check_ctrader_intervals`]'s shape does not
/// transfer): IG's bars are REAL venue candles on the lossless `close_bar` lane through
/// `CoreLaneSink` (`CandleFold` closes a bar on `CONS_END` or a `UTM` advance, whichever arrives
/// first), so two IG mounts at different intervals subscribe two independent TLCP sessions and
/// both bar lanes fire. The ctrader gate exists because a synthesizer has exactly one window; this
/// venue has none.
pub fn ig_plan(cfg: &MakerConfig) -> Result<VenuePlan, String> {
    let slug = "ig";
    let Some(wired) = wired_symbol(slug) else {
        return Err(format!(
            "{slug} is live-wired for a feed but `assemble_runtime` builds no engine for it — orders \
             would have nowhere to go"
        ));
    };
    if cfg.token_id != wired {
        return Err(format!(
            "{slug} is mounted on {wired:?} by assemble_runtime, but this mount names {:?}: that engine \
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
    Ok(VenuePlan::Ig)
}

/// The ctrader same-venue interval gate (split-plane I9) — the ctrader spelling of
/// `check_poly_token_intervals`'s (in `main.rs`) constraint: every ctrader mount shares ONE wired symbol
/// (`EURUSD` — [`ctrader_plan`] refused anything else per row), so all of them share one
/// [`vike_tradehub_core::MakerSink`] whose `TickBarSynthesizer` runs at exactly ONE window. Rows naming
/// different intervals would leave every mount after the first with a silently dead bar lane —
/// no strategy bars, no watchdog cadence. Refused by ROW, before any core spawns.
///
/// Pure over the lowered mount configs so the refusal is unit-testable without a `ResolvedAdapter`
/// (which carries a constructed strategy).
pub fn check_ctrader_intervals(cfgs: &[&MakerConfig]) -> Result<(), String> {
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
