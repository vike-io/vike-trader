//! **THE VENUE WIRING TABLE** — how the trading daemon wires each roster venue: the recon/exec
//! class `venues.json` publishes as `wiring` and, for a venue the daemon mounts an engine for, the
//! symbol that DEFAULT engine mounts on and where the engine sits in the core.
//!
//! ONE table, read by every crate that used to keep its own copy of it: `vike_tradehub_core::wired_markets`
//! turns its engine rows into the markets a build mounts (keeping a row whose registry row is a
//! `Mount` in that build, dropping a `FeatureAbsent` one); `vike-docs` renders `Wiring::as_str`;
//! five bridges scope their startup credential probe to the row's symbol (each one's
//! `PROBE_SYMBOL`, a `const` read through `wired_symbol`); the daemon's profile and feed gates read
//! `wired_symbol` too. It came down from `vike-docs`' `WIRING` and `vike-tradehub`'s per-venue
//! `WiredMarket` consts (decision 0158), as `docs/decisions/0098-vike-run-merges-into-vike-mount.md`'s "What would
//! reopen this" prescribes once a table has more than one consumer: below all of them, never
//! copied.
//!
//! **ROW order is MOUNT order**: the order the node mounts venues in and the order
//! `vike_tradehub_core::armed_live_venues` claims live-account locks in. `engine_rank` is ENGINE order: the
//! order the core holds the default engines in (the smallest is the primary) and lists their
//! reconcile legs in. The two differ for deribit, mounted fourth and held sixth, because that is
//! how the hand-unrolled assembly was written; decision 0098 keeps both.
//!
//! **A row with an engine ARMS the venue in the daemon's node.** The scaffold's placeholder is
//! `Wiring::PaperOnly`, which carries none. Give a venue an engine only once its bridge's mount
//! can arm live, on a pair that venue's own live smoke actually drives (a guessed ticker mounts a
//! feed nobody watches); if the arm is feature-gated, the class is `Wiring::FeatureGated` and the
//! daemon's registry row (`crates/vike-gatewayhub/src/registry/trade.rs`'s `REGISTRY`) takes the matching
//! `#[cfg]` pair in the same PR. Whether a profile may NAME the venue is a second question,
//! answered by the daemon's feed arms (`crates/vike-tradehub/src/feeds.rs`'s `venue_feed_plan`).
//!
//! Playbook rules (`crates/vike-venues/CLAUDE.md`, "Per-venue capability maps (the playbook)"): one
//! NAMED row per [`vike_model::VENUES`] entry, a roster-completeness test and a verbatim pin
//! (`venue_wiring_tests.rs`), and a `just new-venue` marker.

/// How a venue's EXEC and RECONCILE sides are wired into the trading daemon — the `wiring` field
/// of `venues.json` and, for the two classes the daemon mounts, the engine it mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wiring {
    /// The venue's registry row (`crates/vike-gatewayhub/src/registry/trade.rs`'s `REGISTRY`) is an
    /// unconditional `VenueRow::Adapter` whose mount builds a real `ExecutionClient` from
    /// credentials, and the daemon mounts its default engine (this variant's `symbol` and
    /// `engine_rank`), so the venue is in the default reconcile set.
    LiveExecRecon { symbol: &'static str, engine_rank: u8 },
    /// The venue's mount is registered only under a vike-tradehub cargo feature (`ibkr` /
    /// `polymarket` / `fxcm`); a build without it registers the venue `FeatureAbsent`, and
    /// `vike_tradehub_core::wired_markets` drops the row, so that build mounts no engine for it.
    /// Reconcile, where wired, sits behind the same feature (plus each venue's own inner gates).
    FeatureGated { symbol: &'static str, engine_rank: u8 },
    /// Exec runs through an owned child process (the dukascopy JForex Java sidecar over JSON-lines
    /// stdio). Its mount (`DukascopyVenueAdapter` in `crates/bridges/dukascopy/src/adapter.rs`) derives
    /// a positions-only `ReconClient` from the running sidecar, but NOTHING in production mounts
    /// the venue — this variant carries no engine — so it is in no live reconcile set: a
    /// `ReconClient` existing is not the same as being in one. The variant and its published string
    /// are kept — renaming a published value is its own change.
    SidecarExecNoRecon,
    /// Only the paper fallback mounts it — no live exec arm anywhere. No roster venue today; the
    /// variant exists so the class is expressible, and it is the scaffold's conservative
    /// placeholder for a venue whose bridge just landed (no engine, so the daemon arms nothing).
    PaperOnly,
}

impl Wiring {
    /// The kebab-case string `venues.json` carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Wiring::LiveExecRecon { .. } => "live-exec-recon",
            Wiring::FeatureGated { .. } => "feature-gated",
            Wiring::SidecarExecNoRecon => "sidecar-exec-no-recon",
            Wiring::PaperOnly => "paper-only",
        }
    }

    /// The default engine the daemon mounts for this venue, `(symbol, engine_rank)`; `None` for a
    /// class the daemon mounts nothing for. `engine_rank` is the engine's position in the core and
    /// of its reconcile leg: ascending, smallest = primary, distinct per table.
    #[must_use]
    pub const fn engine(self) -> Option<(&'static str, u8)> {
        match self {
            Wiring::LiveExecRecon { symbol, engine_rank }
            | Wiring::FeatureGated { symbol, engine_rank } => Some((symbol, engine_rank)),
            Wiring::SidecarExecNoRecon | Wiring::PaperOnly => None,
        }
    }
}

/// The wiring of every roster venue, in MOUNT order (the module doc).
///
/// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is re-indented
/// by rustfmt once a row ending in a trailing `//` comment is generated above it, which defeats
/// `--remove`. Gated by `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
/// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
#[rustfmt::skip]
pub const VENUE_WIRING: &[(&str, Wiring)] = &[
    // Binance, the PRIMARY engine: the first row, mounted first and held first (`engine_rank: 0`).
    // Its resync supervisor takes the recon trigger.
    ("binance", Wiring::LiveExecRecon { symbol: "BTCUSDT", engine_rank: 0 }),
    // Bybit. Its resync supervisor takes the recon trigger.
    ("bybit", Wiring::LiveExecRecon { symbol: "BTCUSDT", engine_rank: 1 }),
    // OKX. Its resync supervisor takes the recon trigger.
    ("okx", Wiring::LiveExecRecon { symbol: "BTC-USDT-SWAP", engine_rank: 2 }),
    // Deribit, mounted so options orders (from the chain-click → confirm ticket) route to its live
    // exec engine. The future-kind fills channel still reconciles option fills via
    // `get_user_trades`; broadening the live fills channel to `any` kind is a documented follow-up.
    // Deribit's `ReconClient` IS wired — but inside its bridge's own mount, which `make_engine`
    // reaches through the registry (`crates/bridges/deribit/src/adapter.rs`'s `DeribitVenueAdapter`: a
    // dedicated authed order-WS). It takes no recon trigger, so it reconciles on the periodic
    // interval only, not when its socket reconnects.
    //
    // The one row whose two orders differ: mounted FOURTH (its row position here) and
    // held SIXTH (`engine_rank: 5`), because that is how the hand-unrolled assembly was written.
    // Both orders are behaviour, and both are kept (docs/decisions/0098).
    ("deribit", Wiring::LiveExecRecon { symbol: "BTC-PERPETUAL", engine_rank: 5 }),
    // Hyperliquid: LIVE (testnet for a `demo` account; an ACTIVE `live` hyperliquid account row for
    // mainnet — decisions 0095 and 0119, `Network::for_tier`) when the key is in the store, else
    // paper. Perp "BTC"
    // mounted; spot orders route here too. Its reconcile client is `Some` on the live path —
    // `vike_hyperliquid::adapter::live_runtime_for_account` builds `HyperliquidReconClient` via its
    // bespoke signer/transport, and `crates/bridges/hyperliquid/src/adapter.rs`'s
    // `HyperliquidVenueAdapter` hands it back through the mount contract (docs/decisions/0096), so HL
    // joins the node's `recon_clients` and reconciles on the periodic timer. It ALSO takes the
    // recon trigger: HL's exec pump fires it on every WS reconnect so a reconcile pass re-syncs state
    // missed while the socket was down (like the binance/bybit/okx venues).
    ("hyperliquid", Wiring::LiveExecRecon { symbol: "BTC", engine_rank: 3 }),
    // Aster USDⓈ-M perp (`.P` → fapi). LIVE MAINNET exec when a `live`-tier account holds its
    // `USER` + `PRIVATE_KEY` fields (real money — see `AsterVenueAdapter` in
    // `crates/bridges/aster/src/adapter.rs`), TESTNET for a `demo`-tier account holding them, else paper
    // (an account holds ONE key set at its ONE tier, and a live account never falls back to another
    // tier's keys). Its reconcile client is the AsterReconClient (built by that mount
    // from the resolved env), so aster reconciles whenever the reconcile gate is on. No recon trigger
    // (aster's spawn takes none) → periodic/startup reconcile only, not event-driven.
    ("aster", Wiring::LiveExecRecon { symbol: "BTCUSDT.P", engine_rank: 4 }),
    // Alpaca US-equity broker ("AAPL"). LIVE (sandbox) exec when a `demo` alpaca account holds its
    // `CLIENT_ID` + `CLIENT_SECRET` + `ACCOUNT_ID` fields, else
    // paper (see `crates/bridges/alpaca/src/adapter.rs`'s `AlpacaVenueAdapter` — its market feed is
    // `crates/vike-tradehub/src/feeds.rs`'s alpaca arm (split-plane I9)). Its reconcile
    // client is the `ReconClient` that mount builds (a dedicated Bearer `AlpacaRest` on a SECOND
    // OAuth2 lifecycle), so alpaca reconciles whenever the reconcile gate is on. No recon trigger →
    // periodic/startup reconcile only, like deribit/aster — and with no feed it has no feed status,
    // so its health gate reads Healthy (never blocked), like deribit.
    ("alpaca", Wiring::LiveExecRecon { symbol: "AAPL", engine_rank: 6 }),
    // cTrader FX/CFD (EUR/USD demo). LIVE exec when a `demo` ctrader account holds its OAuth grant
    // (and the venue its application pair), else
    // paper (see `crates/bridges/ctrader/src/adapter.rs`'s `CtraderVenueAdapter` — its market feed is
    // `crates/vike-tradehub/src/feeds.rs`'s ctrader arm (split-plane I9)). Its reconcile
    // client is the `ReconClient` that mount builds (a dedicated authed protobuf socket), so ctrader
    // reconciles whenever the reconcile gate is on. No recon trigger → periodic/startup reconcile
    // only, like deribit/aster — and with no feed status its health gate reads Healthy (never
    // blocked), exactly like deribit.
    ("ctrader", Wiring::LiveExecRecon { symbol: "EURUSD", engine_rank: 7 }),
    // IG (IG Group) FX/CFD — the EUR/USD mini epic on the demo account. LIVE exec when a `demo` IG
    // account holds its config fields, else paper (see `crates/bridges/ig/src/adapter.rs`'s
    // `IgVenueAdapter` — its market feed is `crates/vike-tradehub/src/feeds.rs`'s ig arm
    // (split-plane I9)). Its reconcile client is the `ReconClient` that mount
    // builds (a dedicated logged-in `IgSession`), so IG joins the node's `recon_clients` and
    // reconciles whenever the reconcile gate is on. No recon trigger → periodic/startup reconcile
    // only, like deribit/aster/alpaca/ctrader — and with no feed status its health gate reads Healthy
    // (never blocked), like alpaca/ctrader.
    ("ig", Wiring::LiveExecRecon { symbol: "CS.D.EURUSD.MINI.IP", engine_rank: 8 }),
    // OANDA v20 FX — EUR/USD on the demo/fxPractice account. LIVE exec when a `demo` oanda account
    // holds its config fields, else paper (see oanda's bridge mount, `OandaVenueAdapter` — its market
    // feed is `crates/vike-tradehub/src/feeds.rs`'s oanda arm (split-plane I9)). Its reconcile
    // client is the `ReconClient` that mount builds (a
    // dedicated Bearer `OandaRest` on its own `/summary`-probed session), so OANDA joins the node's
    // `recon_clients` and reconciles whenever the reconcile gate is on. Interval-only reconcile (no
    // recon trigger), like ig/alpaca/ctrader; no feed status → its health gate reads Healthy.
    ("oanda", Wiring::LiveExecRecon { symbol: "EURUSD", engine_rank: 9 }),
    // IBKR US-equity broker ("AAPL.SMART.USD" — the canonical `symbol.exchange.currency` form
    // `parse_simplified` maps to a conId), a FEATURE-GATED live venue. This row is ALWAYS in the
    // table; vike-tradehub's `ibkr` feature (→ `vike-ibkr/ibkr` → the `ibapi` registry crate) is what
    // adds the bridge edge and makes the registry row a `Mount`. With the feature OFF (the default)
    // the registry row is `FeatureAbsent`, `vike_tradehub_core::wired_markets` drops this row so the node
    // wires no ibkr market, and no ibapi is built. LIVE exec when the
    // account's config fields resolve AND a TWS/IB Gateway is running, else paper (an EXEC-ONLY mount -
    // no market feed). Its reconcile client is the cpapi `IbkrReconClient` its bridge's mount builds
    // (`IbkrVenueAdapter` in `crates/bridges/vike-ibkr/src/adapter.rs`; its OWN dedicated CP-Gateway
    // transport), so ibkr joins the node's `recon_clients` and reconciles whenever the reconcile gate
    // is on. No recon trigger → periodic/startup reconcile only, like deribit/aster/alpaca — and with
    // no feed status its health gate reads Healthy (never blocked), like deribit/alpaca.
    ("ibkr", Wiring::FeatureGated { symbol: "AAPL.SMART.USD", engine_rank: 10 }),
    // Polymarket, behind vike-tradehub's `polymarket` feature, which registers
    // `crates/bridges/polymarket/src/exec_plane/adapter.rs`'s `PolymarketVenueAdapter`. The symbol is
    // EMPTY on purpose: this venue's mount is ACCOUNT-WIDE — exec, the user-WS fill pump and
    // reconcile all key off the wallet, not a market — and the mount reads the symbol only to seed
    // its neg-risk lookup, which resolves per token on demand anyway. Mounting one hardcoded 5-minute
    // window would be actively wrong: `btc-updown-5m-*` markets expire every 300s, so any literal
    // here is stale within the minute.
    //
    // ⚠ The feature only makes the registry row a `Mount`, so `vike_tradehub_core::wired_markets` keeps
    // this row (the row itself is always in the table). What it mounts is decided inside that mount by
    // `flags.poly_exec` / `flags.poly_reconcile` in the credential map it is handed (the daemon folds
    // both rows in), both default-off ⇒ paper engine, no network call. Its reconcile handle is `None`
    // when `flags.poly_reconcile` is off or creds are absent, and it has no feed status, so its health
    // gate reads Healthy — interval-only reconcile, like deribit/alpaca/ibkr.
    //
    // ⚠ Run it under `config.reconcile_policy = quarantine` UNLESS `flags.poly_exec` is also on: with exec
    // on paper, `hybrid` auto-applies `PositionDrift` and folds the LIVE account's position into the
    // PAPER engine's books at the venue's avg price. (A comment in the node assembly once blamed
    // `OrphanLocalOrder` "auto-cancelling every pass" — false: that kind resolves to zero events
    // under every policy. See `crates/vike-exec/tests/recon/recon_policy_pin.rs`.) See the root
    // `CLAUDE.md`'s *Reconciliation engine* Polymarket bullet.
    ("polymarket", Wiring::FeatureGated { symbol: "", engine_rank: 11 }),
    // FXCM ForexConnect FX (EUR/USD), behind vike-tradehub's `fxcm` feature — the one feature-gated
    // venue whose live gate is a property of the BOX. The feature registers `FxcmVenueAdapter` in
    // `crates/bridges/fxcm/src/adapter.rs`; the mount goes live only when a `demo` fxcm account's config
    // fields resolve AND `vike_fxcm::sdk_available()` is true. On a box where the ForexConnect shim does
    // not load, the mount REFUSES to go live with an `error!` and lands on paper. Every CI runner
    // takes the refusing branch, because none of them has the SDK. EUR/USD because it is the pair the
    // demo account trades and the one both live smokes drive; `crates/bridges/fxcm/src/exec.rs`'s
    // `to_fxcm_instrument` maps it to the venue's slashed `EUR/USD`.
    //
    // ⚠ Before fxcm had a wiring row the feature linked the SDK and wired NOTHING: the node assembly
    // had no fxcm call and the table no fxcm row, so an operator who installed the `vike-tradehub-fxcm`
    // release asset reasonably believed FXCM was live and it was not, with no error anywhere. (That
    // asset is gone: the shipped `vike` carries this arm, and whether it arms is a property of the box
    // where `just fxcm-package` put the shim beside the SDK libraries.) The row and its
    // call landed in the same branch as the sizing conversion
    // (`crates/bridges/fxcm/src/event_mapper.rs`'s `lots_for`) — deliberately, because before it
    // `qty` was read as a LOT COUNT while every caller sends base units, and wiring the venue without
    // it would have made a thousand-fold oversize reachable with real money.
    //
    // EXEC-ONLY, like alpaca/ctrader/ig/oanda/ibkr: this bridge has no market-data seam at all
    // (`LiveDataCaps::NONE`, `NoPump`), so this daemon's feed wiring (`vike_tradehub::feeds`) builds no feed
    // for it and its health gate reads Healthy. Its reconcile client is the `FxcmReconClient` its
    // bridge's mount builds, on its OWN dedicated ForexConnect session, so it joins the node's
    // `recon_clients` and reconciles whenever the reconcile gate is on; no recon trigger ->
    // periodic/startup only.
    //
    // ⚠ RECONCILE IS NOT OPTIONAL IN PRACTICE HERE, and that is this venue's own caps row talking:
    // the async fill lane routes on an in-process map built at accept time, so across a RESTART
    // every re-surfaced trade is unroutable and dropped (at `warn!`). `fetch_fill_reports` reading
    // the same Trades table by the same `trade_id` is what recovers them. Mounting fxcm with reconcile
    // refused (`flags.reconcile_off`) means accepting the loss of every fill that lands across a
    // restart.
    ("fxcm", Wiring::FeatureGated { symbol: "EURUSD", engine_rank: 12 }),
    // No engine: argued on `Wiring::SidecarExecNoRecon` itself.
    ("dukascopy", Wiring::SidecarExecNoRecon),
    // vike:new-venue:row ("{venue}", Wiring::PaperOnly), // TODO(new-venue: {venue}): classify the recon/exec wiring — PaperOnly is the scaffold's conservative placeholder (no engine, the daemon arms nothing), not a verdict
];

/// The declared [`Wiring`] for a canonical venue string; `None` for anything not in
/// [`VENUE_WIRING`]. `None` for a ROSTER venue is a test failure (`venue_wiring_tests.rs`'s
/// `the_wiring_table_is_roster_complete`), never a state a reader should tolerate.
#[must_use]
pub fn wiring_for(venue: &str) -> Option<Wiring> {
    VENUE_WIRING.iter().find(|(v, _)| *v == venue).map(|&(_, w)| w)
}

/// THE "venue → symbol" lookup: the symbol `venue`'s default engine mounts on; `None` for a venue
/// with no engine or one not on the roster. Every reader asks this — the bridges' probe symbols,
/// the daemon's profile gate (`DaemonProfile::validate_for_live`) and its feed plans — so there is
/// no second lookup to drift. `const` so a bridge declares its probe symbol as a compile-time read:
/// `const PROBE_SYMBOL: &str = match wired_symbol(VENUE) { Some(s) => s, None => panic!(…) };` —
/// and a row that loses its engine fails that bridge's BUILD rather than a test.
#[must_use]
pub const fn wired_symbol(venue: &str) -> Option<&'static str> {
    let mut i = 0;
    while i < VENUE_WIRING.len() {
        let (v, w) = VENUE_WIRING[i];
        if same_str(v, venue) {
            return match w.engine() {
                Some((symbol, _)) => Some(symbol),
                None => None,
            };
        }
        i += 1;
    }
    None
}

/// Byte equality usable in a `const fn` (`str`'s `==` is not `const`).
const fn same_str(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
#[path = "venue_wiring_tests.rs"]
mod tests;
