//! **THE WIRED MARKETS** — the venues this daemon's node mounts an engine for, the symbol each
//! venue's DEFAULT engine mounts on, whether its mount is handed the reconnect trigger (which must
//! equal its bridge's own `takes_recon_trigger` declaration; a test below holds the two equal), and
//! where its default engine sits in the core. Handed to `vike_mount::build_node` as
//! `NodeConfig::markets`, beside [`crate::registry::REGISTRY`]
//! (docs/decisions/0098-vike-run-merges-into-vike-mount.md). Moved here from vike-run's node
//! assembly, which named every venue in it.
//!
//! TABLE order is MOUNT order: the order the node mounts venues in and the order
//! `armed_live_venues` claims live-account locks in. `engine_rank` is ENGINE order: the order the
//! core holds the default engines in (the smallest is the primary) and lists their reconcile
//! legs. The two differ for deribit, which is mounted fourth and held sixth, because that is how
//! the hand-unrolled assembly was written; both are kept rather than either being changed.
//!
//! Extending the daemon to another venue is a row here AND its entry in
//! [`crate::config::DaemonProfile::validate_for_live`]'s allow-list
//! ([`crate::config::LIVE_WIRED_VENUES`]), both in this crate;
//! `crates/vike-tradehub/src/config/tests/wired_set_sync.rs` holds the two in step.
//!
//! "The reconcile gate", wherever a row below says a venue reconciles when it is on, is
//! [`crate::reconcile_config::reconcile_gate`]: ON by default for a mount that arms at least one
//! live venue account, refused by `flags.reconcile_off` (`VIKE_RECONCILE_OFF=1`, which wins), and
//! force-armed by `flags.reconcile` (`VIKE_RECONCILE=1`) where the armed-live probe reports paper.
//! A row's venue reconciles only where its mount also hands back a reconcile client.
//!
//! ⚠ The tests that pin this table's two orders live at the bottom of THIS file rather than in an
//! integration test, and the reason is a gate, not taste: a file that spells the wired venues in
//! order names thirteen of the fourteen roster ids, which `crates/vike-ops/tests/venues/new_venue_gate.rs`
//! reads as a per-venue table needing a scaffold marker. This file already is one and carries the
//! marker; a second file pinning the same list would be a second site for the same table.

use vike_mount::WiredMarket;

// vike:new-venue:note do NOT add a `{VENUE}_MARKET` row or a `WIRED_MARKETS` entry for `{venue}` yet. A row here is what ARMS the venue in this node, and it is only correct once the venue's mount can arm live - the same rule as the bridge's scaffolded `mount.rs`, whose `resolve` answers `NoLiveArm` until the mount is real. When it IS time: the symbol must be a pair that venue's own live smoke actually drives (a guessed ticker mounts a feed nobody watches), it needs a matching entry in this crate's `DaemonProfile::validate_for_live` allow-list in the SAME PR, and if the arm is feature-gated the row takes the same `#[cfg]` so the table equals what this build wires.

/// Binance, the PRIMARY engine: the first row, mounted first and held first (`engine_rank: 0`).
/// Its resync supervisor takes the reconnect poke.
const BINANCE_MARKET: WiredMarket =
    WiredMarket { venue: "binance", symbol: "BTCUSDT", reconnect_poke: true, engine_rank: 0 };

/// Bybit. Its resync supervisor takes the reconnect poke.
const BYBIT_MARKET: WiredMarket =
    WiredMarket { venue: "bybit", symbol: "BTCUSDT", reconnect_poke: true, engine_rank: 1 };

/// OKX. Its resync supervisor takes the reconnect poke.
const OKX_MARKET: WiredMarket =
    WiredMarket { venue: "okx", symbol: "BTC-USDT-SWAP", reconnect_poke: true, engine_rank: 2 };

/// Deribit, mounted so options orders (from the chain-click → confirm ticket) route to its live
/// exec engine. The future-kind fills channel still reconciles option fills via
/// `get_user_trades`; broadening the live fills channel to `any` kind is a documented follow-up.
/// Deribit's `ReconClient` IS wired — but inside its bridge's own mount, which `make_engine`
/// reaches through the registry (`crates/bridges/deribit/src/mount.rs`'s `DeribitVenueMount`: a
/// dedicated authed order-WS). It takes no reconnect poke, so it reconciles on the periodic
/// interval only, not on a reconnect poke.
///
/// The one row whose two orders differ: mounted FOURTH (its position in [`WIRED_MARKETS`]) and
/// held SIXTH (`engine_rank: 5`), because that is how the hand-unrolled assembly was written.
/// Both orders are behaviour, and both are kept (docs/decisions/0098).
const DERIBIT_MARKET: WiredMarket = WiredMarket {
    venue: "deribit",
    symbol: "BTC-PERPETUAL",
    reconnect_poke: false,
    engine_rank: 5,
};

/// Hyperliquid: LIVE (testnet by default; the ceiling `policy.venues.hyperliquid = live` for
/// mainnet — decision 0095, `Env::for_ceiling`) when the key is in .env, else paper. Perp "BTC"
/// mounted; spot orders route here too. Its reconcile client is `Some` on the live path —
/// `vike_hyperliquid::mount::live_mount_for_account` builds `HyperliquidReconClient` via its
/// bespoke signer/transport, and `crates/bridges/hyperliquid/src/mount.rs`'s
/// `HyperliquidVenueMount` hands it back through the mount contract (docs/decisions/0096), so HL
/// joins the node's `recon_clients` and reconciles on the periodic timer. It ALSO takes the
/// reconnect poke: HL's exec pump pokes it on every WS reconnect so a reconcile pass re-syncs state
/// missed while the socket was down (like the binance/bybit/okx venues).
const HYPERLIQUID_MARKET: WiredMarket =
    WiredMarket { venue: "hyperliquid", symbol: "BTC", reconnect_poke: true, engine_rank: 3 };

/// Aster USDⓈ-M perp (`.P` → fapi). LIVE MAINNET exec when `ASTER_LIVE_*` creds are present under
/// a `live` ceiling (real money — see `AsterVenueMount` in `crates/bridges/aster/src/mount.rs`),
/// else TESTNET, else paper. Its reconcile client is the AsterReconClient (built by that mount
/// from the resolved env), so aster reconciles whenever the reconcile gate is on. No reconnect
/// poke (aster's spawn takes none) → periodic/startup reconcile only, not event-driven.
const ASTER_MARKET: WiredMarket =
    WiredMarket { venue: "aster", symbol: "BTCUSDT.P", reconnect_poke: false, engine_rank: 4 };

/// Alpaca US-equity broker ("AAPL"). LIVE exec when the `ALPACA_SANDBOX_*` creds are present, else
/// paper (see `crates/bridges/alpaca/src/mount.rs`'s `AlpacaVenueMount` — an EXEC-ONLY mount; no
/// market feed, so this daemon's feed wiring (`crate::feeds`) builds none for it). Its reconcile
/// client is the `ReconClient` that mount builds (a dedicated Bearer `AlpacaRest` on a SECOND
/// OAuth2 lifecycle), so alpaca reconciles whenever the reconcile gate is on. No reconnect poke →
/// periodic/startup reconcile only, like deribit/aster — and with no feed it has no feed status,
/// so its health gate reads Healthy (never blocked), like deribit.
const ALPACA_MARKET: WiredMarket =
    WiredMarket { venue: "alpaca", symbol: "AAPL", reconnect_poke: false, engine_rank: 6 };

/// cTrader FX/CFD (EUR/USD demo). LIVE exec when the `CTRADER_*` OAuth creds are present, else
/// paper (see `crates/bridges/ctrader/src/mount.rs`'s `CtraderVenueMount` — an EXEC-ONLY mount; no
/// market feed, so this daemon's feed wiring (`crate::feeds`) builds none for it). Its reconcile
/// client is the `ReconClient` that mount builds (a dedicated authed protobuf socket), so ctrader
/// reconciles whenever the reconcile gate is on. No reconnect poke → periodic/startup reconcile
/// only, like deribit/aster — and with no feed status its health gate reads Healthy (never
/// blocked), exactly like deribit.
const CTRADER_MARKET: WiredMarket =
    WiredMarket { venue: "ctrader", symbol: "EURUSD", reconnect_poke: false, engine_rank: 7 };

/// IG (IG Group) FX/CFD — the EUR/USD mini epic on the demo account. LIVE exec when the
/// `IG_DEMO_*` config is present, else paper (see `crates/bridges/ig/src/mount.rs`'s
/// `IgVenueMount` — an EXEC-ONLY mount; no market feed, so this daemon's feed wiring
/// (`crate::feeds`) builds none for it). Its reconcile client is the `ReconClient` that mount
/// builds (a dedicated logged-in `IgSession`), so IG joins the node's `recon_clients` and
/// reconciles whenever the reconcile gate is on. No reconnect poke → periodic/startup reconcile
/// only, like deribit/aster/alpaca/ctrader — and with no feed status its health gate reads Healthy
/// (never blocked), like alpaca/ctrader.
const IG_MARKET: WiredMarket = WiredMarket {
    venue: "ig",
    symbol: "CS.D.EURUSD.MINI.IP",
    reconnect_poke: false,
    engine_rank: 8,
};

/// OANDA v20 FX — EUR/USD on the demo/fxPractice account. LIVE exec when the `OANDA_DEMO_*`
/// config is present, else paper (see oanda's bridge mount, `OandaVenueMount` — an EXEC-ONLY
/// mount; no market feed). Its reconcile client is the `ReconClient` that mount builds (a
/// dedicated Bearer `OandaRest` on its own `/summary`-probed session), so OANDA joins the node's
/// `recon_clients` and reconciles whenever the reconcile gate is on. Interval-only reconcile (no
/// reconnect poke), like ig/alpaca/ctrader; no feed status → its health gate reads Healthy.
const OANDA_MARKET: WiredMarket =
    WiredMarket { venue: "oanda", symbol: "EURUSD", reconnect_poke: false, engine_rank: 9 };

/// IBKR US-equity broker ("AAPL.SMART.USD" — the canonical `symbol.exchange.currency` form
/// `parse_simplified` maps to a conId), a FEATURE-GATED live venue. This row exists ONLY under
/// this crate's `ibkr` feature (→ `vike-ibkr/ibkr` → the `ibapi` registry crate), the same feature
/// that adds the bridge edge and the registry row; with the feature OFF (the default) the registry
/// row is `FeatureAbsent`, the node wires no ibkr market, and no ibapi is built. LIVE exec when the
/// `IBKR_DEMO_*` config resolves AND a TWS/IB Gateway is running, else paper (an EXEC-ONLY mount -
/// no market feed). Its reconcile client is the cpapi `IbkrReconClient` its bridge's mount builds
/// (`IbkrVenueMount` in `crates/bridges/vike-ibkr/src/mount.rs`; its OWN dedicated CP-Gateway
/// transport), so ibkr joins the node's `recon_clients` and reconciles whenever the reconcile gate
/// is on. No reconnect poke → periodic/startup reconcile only, like deribit/aster/alpaca — and with
/// no feed status its health gate reads Healthy (never blocked), like deribit/alpaca.
#[cfg(feature = "ibkr")]
const IBKR_MARKET: WiredMarket =
    WiredMarket { venue: "ibkr", symbol: "AAPL.SMART.USD", reconnect_poke: false, engine_rank: 10 };

/// Polymarket, behind this crate's `polymarket` feature, which registers
/// `crates/bridges/polymarket/src/exec_plane/mount.rs`'s `PolymarketVenueMount`. The symbol is
/// EMPTY on purpose: this venue's mount is ACCOUNT-WIDE — exec, the user-WS fill pump and
/// reconcile all key off the wallet, not a market — and the mount reads the symbol only to seed
/// its neg-risk lookup, which resolves per token on demand anyway. Mounting one hardcoded 5-minute
/// window would be actively wrong: `btc-updown-5m-*` markets expire every 300s, so any literal
/// here is stale within the minute.
///
/// ⚠ The feature only makes this row EXIST. What it mounts is decided inside that mount by
/// `flags.poly_exec` / `flags.poly_reconcile` in the credential map it is handed (the daemon folds
/// both rows in), both default-off ⇒ paper engine, no network call. Its reconcile handle is `None`
/// when `flags.poly_reconcile` is off or creds are absent, and it has no feed status, so its health
/// gate reads Healthy — interval-only reconcile, like deribit/alpaca/ibkr.
///
/// ⚠ Run it under `VIKE_RECONCILE_POLICY=quarantine` UNLESS `flags.poly_exec` is also on: with exec
/// on paper, `hybrid` auto-applies `PositionDrift` and folds the LIVE account's position into the
/// PAPER engine's books at the venue's avg price. (A comment in the node assembly once blamed
/// `OrphanLocalOrder` "auto-cancelling every pass" — false: that kind resolves to zero events
/// under every policy. See `crates/vike-exec/tests/recon/recon_policy_pin.rs`.) See the root
/// `CLAUDE.md`'s *Reconciliation engine* Polymarket bullet.
#[cfg(feature = "polymarket")]
const POLYMARKET_MARKET: WiredMarket =
    WiredMarket { venue: "polymarket", symbol: "", reconnect_poke: false, engine_rank: 11 };

/// FXCM ForexConnect FX (EUR/USD), behind this crate's `fxcm` feature — the one feature-gated
/// venue whose live gate is a property of the BOX. The feature registers `FxcmVenueMount` in
/// `crates/bridges/fxcm/src/mount.rs`; the mount goes live only when the `FXCM_DEMO_*` config
/// resolves AND `vike_fxcm::sdk_available()` is true. On a box where the ForexConnect shim does
/// not load, the mount REFUSES to go live with an `error!` and lands on paper. Every CI runner
/// takes the refusing branch, because none of them has the SDK. EUR/USD because it is the pair the
/// demo account trades and the one both live smokes drive; `crates/bridges/fxcm/src/exec.rs`'s
/// `to_fxcm_instrument` maps it to the venue's slashed `EUR/USD`.
///
/// ⚠ Until this row existed the feature linked the SDK and wired NOTHING: the node assembly had no
/// fxcm call and the table no fxcm row, so an operator who installed the `vike-tradehub-fxcm`
/// release asset reasonably believed FXCM was live and it was not, with no error anywhere. (That
/// asset is gone: the shipped `vike` carries this arm, and whether it arms is a property of the box
/// where `just fxcm-package` put the shim beside the SDK libraries.) The row and its
/// call landed in the same branch as the sizing conversion
/// (`crates/bridges/fxcm/src/event_mapper.rs`'s `lots_for`) — deliberately, because before it
/// `qty` was read as a LOT COUNT while every caller sends base units, and wiring the venue without
/// it would have made a thousand-fold oversize reachable with real money.
///
/// EXEC-ONLY, like alpaca/ctrader/ig/oanda/ibkr: this bridge has no market-data seam at all
/// (`LiveDataCaps::NONE`, `NoPump`), so this daemon's feed wiring (`crate::feeds`) builds no feed
/// for it and its health gate reads Healthy. Its reconcile client is the `FxcmReconClient` its
/// bridge's mount builds, on its OWN dedicated ForexConnect session, so it joins the node's
/// `recon_clients` and reconciles whenever the reconcile gate is on; no reconnect poke ->
/// periodic/startup only.
///
/// ⚠ RECONCILE IS NOT OPTIONAL IN PRACTICE HERE, and that is this venue's own caps row talking:
/// the async fill lane routes on an in-process map built at accept time, so across a RESTART
/// every re-surfaced trade is unroutable and dropped (at `warn!`). `fetch_fill_reports` reading
/// the same Trades table by the same `trade_id` is what recovers them. Mounting fxcm with reconcile
/// refused (`flags.reconcile_off`) means accepting the loss of every fill that lands across a
/// restart.
#[cfg(feature = "fxcm")]
const FXCM_MARKET: WiredMarket =
    WiredMarket { venue: "fxcm", symbol: "EURUSD", reconnect_poke: false, engine_rank: 12 };

/// The wired markets, in MOUNT order. The optional venues' rows exist only under this crate's
/// features, exactly like their registry rows, so the table always equals what this build
/// wires.
///
/// It is also the table the OTHER place that names live-wired pairs is completeness-tested against:
/// [`crate::config::LIVE_WIRED_VENUES`] (allow-list ⊆ this set, the CLAUDE.md capability-map
/// STEP-1 pattern), so extending the daemon to another venue stays a deliberate two-place edit,
/// both in this crate, never a silent widening.
pub const WIRED_MARKETS: &[WiredMarket] = &[
    BINANCE_MARKET,
    BYBIT_MARKET,
    OKX_MARKET,
    DERIBIT_MARKET,
    HYPERLIQUID_MARKET,
    ASTER_MARKET,
    ALPACA_MARKET,
    CTRADER_MARKET,
    IG_MARKET,
    OANDA_MARKET,
    #[cfg(feature = "ibkr")]
    IBKR_MARKET,
    #[cfg(feature = "polymarket")]
    POLYMARKET_MARKET,
    #[cfg(feature = "fxcm")]
    FXCM_MARKET,
];

#[cfg(test)]
mod tests {
    use super::WIRED_MARKETS;

    /// MOUNT order: the order `build_node` mounts venues in and `armed_live_venues` claims
    /// live-account locks in. Pinned before docs/decisions/0098 moved the table; it must not move.
    #[test]
    fn the_wired_markets_are_mounted_in_this_order() {
        let mut want = vec![
            "binance",
            "bybit",
            "okx",
            "deribit",
            "hyperliquid",
            "aster",
            "alpaca",
            "ctrader",
            "ig",
            "oanda",
        ];
        if cfg!(feature = "ibkr") {
            want.push("ibkr");
        }
        if cfg!(feature = "polymarket") {
            want.push("polymarket");
        }
        if cfg!(feature = "fxcm") {
            want.push("fxcm");
        }
        let got: Vec<&str> = WIRED_MARKETS.iter().map(|m| m.venue).collect();
        assert_eq!(got, want);
    }

    /// ENGINE order: the order the core holds the default engines in (`CoreSnapshot::portfolio`'s
    /// `venues`, and so every per-venue `py_sum`) and lists their default reconcile legs in.
    /// Ascending `engine_rank`, the smallest being the primary. Pinned with the move
    /// (docs/decisions/0098); it is the order the hand-unrolled assembly built `extra` and
    /// `recon_clients` in.
    #[test]
    fn the_core_holds_the_default_engines_in_rank_order() {
        let mut ranked: Vec<_> = WIRED_MARKETS.to_vec();
        ranked.sort_by_key(|m| m.engine_rank);
        let mut want = vec![
            "binance",
            "bybit",
            "okx",
            "hyperliquid",
            "aster",
            "deribit",
            "alpaca",
            "ctrader",
            "ig",
            "oanda",
        ];
        if cfg!(feature = "ibkr") {
            want.push("ibkr");
        }
        if cfg!(feature = "polymarket") {
            want.push("polymarket");
        }
        if cfg!(feature = "fxcm") {
            want.push("fxcm");
        }
        let got: Vec<&str> = ranked.iter().map(|m| m.venue).collect();
        assert_eq!(got, want);

        let mut ranks: Vec<u8> = WIRED_MARKETS.iter().map(|m| m.engine_rank).collect();
        ranks.sort_unstable();
        let n = ranks.len();
        ranks.dedup();
        assert_eq!(
            ranks.len(),
            n,
            "two rows share an engine_rank: their order would fall to the table"
        );
        let smallest = WIRED_MARKETS.iter().map(|m| m.engine_rank).min();
        assert_eq!(
            WIRED_MARKETS.first().map(|m| m.engine_rank),
            smallest,
            "the FIRST row mounts first and is held as the primary engine: it carries the smallest \
             rank"
        );
    }

    /// The venues whose resync supervisor takes the shared reconnect poke. Every other wired venue
    /// reconciles on the interval only.
    #[test]
    fn exactly_four_venues_take_the_reconnect_poke() {
        let mut poked: Vec<&str> =
            WIRED_MARKETS.iter().filter(|m| m.reconnect_poke).map(|m| m.venue).collect();
        poked.sort_unstable();
        assert_eq!(poked, ["binance", "bybit", "hyperliquid", "okx"]);
    }

    /// The row's `reconnect_poke` is only half of the effective poke: `build_node` hands the
    /// trigger to the mount, and the contract passes it on to the bridge only when the bridge's
    /// declaration asks (`crates/vike-mount/src/contract.rs`'s `takes_recon_trigger` check). So
    /// every wired row must agree with its own registry row: a row that pokes a bridge that takes
    /// no trigger is inert, and a bridge that takes one from a row that does not poke silently
    /// loses its reconnect-driven reconcile. The test above pins the row alone; this pins the pair,
    /// in every feature combination this table has rows for.
    #[test]
    fn every_rows_reconnect_poke_matches_its_bridges_declaration() {
        for m in WIRED_MARKETS {
            let row = crate::registry::REGISTRY
                .iter()
                .find(|r| r.venue() == m.venue)
                .unwrap_or_else(|| panic!("{} is wired but has no registry row", m.venue));
            let vike_mount::VenueRow::Mount(mount) = row else {
                panic!("{} is wired but registered FeatureAbsent in this build", m.venue);
            };
            assert_eq!(
                m.reconnect_poke,
                mount.declaration().takes_recon_trigger,
                "{}: the wired row's `reconnect_poke` and the bridge's `takes_recon_trigger` \
                 disagree, so the effective reconnect poke is not what the row says",
                m.venue
            );
        }
    }

    /// Table sanity (audit F5): one row per venue — a duplicate venue would make [`WIRED_MARKETS`]
    /// ambiguous as the authority downstream completeness tests key on — and every symbol is
    /// non-empty EXCEPT polymarket's deliberately account-wide empty one.
    #[test]
    fn wired_markets_venues_are_unique_and_symbols_shaped() {
        let mut venues: Vec<&str> = WIRED_MARKETS.iter().map(|m| m.venue).collect();
        let n = venues.len();
        venues.sort_unstable();
        venues.dedup();
        assert_eq!(venues.len(), n, "duplicate venue row in WIRED_MARKETS");
        for m in WIRED_MARKETS {
            let (venue, symbol) = (m.venue, m.symbol);
            if venue == "polymarket" {
                assert!(symbol.is_empty(), "polymarket's mount is account-wide (empty symbol)");
            } else {
                assert!(!symbol.is_empty(), "wired venue {venue} must name its mounted symbol");
            }
        }
    }

    /// Every wired venue is on the canonical roster (`vike_model::VENUES`) — the same tie-in every
    /// capability table's completeness test uses, so a typo'd venue id here cannot silently mount
    /// nothing.
    #[test]
    fn wired_markets_venues_are_on_the_canonical_roster() {
        for m in WIRED_MARKETS {
            let venue = m.venue;
            assert!(
                vike_model::VENUES.contains(&venue),
                "WIRED_MARKETS names {venue}, which is not in vike_model::VENUES"
            );
        }
    }

    /// The xEMM v1 pair against the REAL table — the half of the xEMM validation tests that
    /// asserted the daemon's wiring rather than the rule (the rule's own tests plant a table, since
    /// the crate that owns `XemmMountConfig::validate` holds no venue). The v1 pair validates to
    /// the 6.5 bp round-trip fee bit for bit, and a hedge symbol okx is not wired for is refused
    /// by name.
    #[test]
    fn the_xemm_v1_pair_is_wired_in_the_real_table() {
        let v1 = || {
            vike_mount::XemmMountConfig::crypto(
                "hyperliquid",
                "BTC",
                "okx",
                "BTC-USDT-SWAP",
                0.01,
                0.0005,
                0.5,
            )
        };
        let fee =
            v1().validate(Some(WIRED_MARKETS)).expect("the v1 pair is wired and fee-expressible");
        assert_eq!(fee.to_bits(), (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits());

        let mut cfg = v1();
        cfg.hedge_symbol = "ETH-USDT-SWAP".into();
        assert_eq!(
            cfg.validate(Some(WIRED_MARKETS)),
            Err(vike_mount::XemmConfigError::HedgeSymbolNotAccepted {
                venue: "okx".into(),
                wired: "BTC-USDT-SWAP".into(),
                requested: "ETH-USDT-SWAP".into(),
            })
        );
    }
}
