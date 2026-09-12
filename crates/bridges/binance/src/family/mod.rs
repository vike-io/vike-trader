//! Shared Binance-wire-grammar core reused by vike-aster — NOT a venue. Never registered as a
//! CatalogProvider or mounted as an ExecutionClient.
//!
//! Aster's API is a near-verbatim Binance fork (same event names, same field letters, same
//! `exchangeInfo` filters, same listenKey user-data transport, same public stream names and
//! depth-sync rule), so its wire code was byte-for-byte copied from Binance's. This module is the
//! ONE home for that shared grammar; the two venues' own modules are thin re-exports/callers over
//! it, each supplying its own venue string and its own host table.
//!
//! **Rung 1 (F0) — the pure mappers.** `event_mapper`/`perp_mapper`/`history`/`filters_rec`:
//! serde_json in, `vike_model` events out, `venue` a caller-supplied parameter at every site.
//!
//! **Rung 2 (F8/F9/F11/F17) — the market-data stack.** `depth` (the U/u depth-sync protocol + the
//! HFT tick pump), `trades` (the aggTrade decode + the WS-buffer→REST-warmup→splice startup dance +
//! backward-paging backfill), and `market_feed` (the live kline `DataClient` feed body + the DOM
//! depth lane). Unlike rung 1 these touch the network, so the ONE real per-venue delta —
//! host/URL resolution — is passed in as a [`UrlTable`] value inside a [`FamilySpec`]: Binance
//! supplies a `const` table ([`crate::market_feed::BINANCE_URLS`], never env-resolved), Aster maps
//! its `urls::urls_for(env)` into the same shape. Nothing here decides which network it is on.
//!
//! **Rung 3 (F14) — the public catalog.** `catalog`: the `exchangeInfo` spot/perp parse both
//! venues' `CatalogProvider`s delegate their `list_instruments` body to. Same discipline as rung 2
//! — the per-venue delta (the two `exchangeInfo` URLs) is passed in as plain `&str`, so Binance's
//! stay `const`s and Aster's stay its own mainnet-always resolution, and nothing here became
//! env-resolved. `CatalogProvider` itself is deliberately NOT implemented here: each venue owns its
//! struct and its `venue()`/`mode()`.
//!
//! **Rung 4a (F13) — the listenKey user-data pump.** `listenkey`: mint the key over REST → connect
//! `<ws_base>[/ws]/<key>` → drive bridge-core's venue-neutral reliability loop, plus the resync
//! supervisor wiring. There were THREE copies of this one shape (Binance perp, Aster perp, Aster
//! spot). The per-venue delta is the listenKey call's AUTH — Binance's `X-MBX-APIKEY` header vs
//! Aster's EIP-712-signed query — passed in as a [`listenkey::ListenKeyAuth`] impl, so each venue
//! keeps only its auth. It lives here rather than in vike-bridge-core deliberately: listenKey is
//! Binance wire grammar and all three copies are family venues, so hoisting it into the neutral
//! core would have broken bridge-core's venue-neutral charter. Hosts/endpoint paths stay
//! caller-supplied, same as rungs 2–3.
//!
//! **Rung 4b — the exec dispatch loop.** `exec_loop`: `split_symbol` (the `.P`-suffix split) and
//! `run_loop` (the command→REST→ingest pump + gap-sentinel) were byte-identical copies in both
//! venues' `exec.rs` (the only textual diffs were doc comments). `run_loop` — generic over the REST
//! surface (`R: VenueRest`) and a resync closure, with ZERO venue-local dependency — has since been
//! hoisted further down to [`vike_bridge_core::exec_actor::run_loop`]: deribit/bybit/okx carried the
//! same verbatim copy, so it is not even family-specific, and all four venues now call the ONE
//! bridge-core copy. `exec_loop` keeps `split_symbol` (family wire grammar). Each venue keeps its
//! own `run`/`run_spot`/`run_perp` drivers (venue REST types, hosts, signers, `tracing` targets).
//! The signed REST METHODS themselves stay per-venue — they name diverging endpoint paths (Binance
//! `/fapi/v1|v2` split vs Aster's single `/fapi/v3`) and per-venue log targets.
//! The pure spot/perp response mappers (`json_id`/`parse_symbol_properties`/the perp open-order
//! row/the order-param builders) were a LARGER but SEPARATE dedup candidate, an `exec_loop`-sibling
//! shared next as rung 4c ([`order_map`]).
//!
//! **Rung 4c — the pure order mappers.** `order_map`: the four pure, signing-/path-independent
//! slices of the signed REST clients that were byte-identical copies in both venues' `spot.rs`/
//! `perp.rs`. `json_id` (numeric-id coercion; four private copies), `parse_symbol_properties` (the
//! spot `/exchangeInfo`→`SymbolProperties` parse, `pub`-re-exported by each `spot`), the spot/perp
//! `build_*_order_params` payload shapers (each venue's `build_order_params` METHOD is now a one-line
//! delegation, `symbol`/`properties` passed in), and `map_perp_open_order` (one resting-perp
//! `/openOrders` row → a reconcile-seeded `ManagedOrder`, `venue` now a parameter — each venue's thin
//! 1-arg wrapper passes `crate::VENUE`, so its public signature and the r6 call sites are
//! unchanged). The param ORDER is load-bearing (r6 fixtures pin the exact byte output), identical
//! between the two venues; `format_to_step_f` stays the pinned Decimal wire site. Left per-venue and
//! NOT folded (each names a diverging path or a crate-local type r6 constructs by name): the
//! crate-local `PerpInstrument` + `parse_*_perp_instruments`, spot `connect`'s inline open-order and
//! reconcile-snapshot mapping, and the submit-event extraction.
//!
//! **Rung 5 — the reconcile client.** `recon`: the venue-facing `ReconClient` report seam (spot +
//! perp order/fill/position/balance/fee reports) was a byte-identical copy in both venues'
//! `recon_client.rs` — the pure `parse_*` body parsers AND the fetch/dispatch client, differing only
//! in the endpoint PATHS (Binance's `/fapi/v1|v2` split vs Aster's single `/fapi/v3`) and the venue
//! label. Both now pass into ONE [`recon::FamilyReconClient`], with the per-venue delta supplied as a
//! [`recon::ReconSpec`] (a [`recon::ReconPaths`] table + the `venue` string), same discipline as
//! rungs 2–4. Each venue keeps a thin named wrapper (`BinanceReconClient`/`AsterReconClient`) pinned
//! to its spec, plus 1-arg `parse_*` re-exports that inject its own `venue`, so the shared parsers
//! stay proven twice against each venue's fixtures. Healed a real drift here: aster's copy predated
//! binance's #416 fee lane, so aster reconcile never refreshed live fees.
//!
//! ⚠ That heal took TWO steps, and the second is why [`recon::ReconPaths`] carries a
//! `spot_commission_rate` slot. Routing aster through the shared client restored the lane's SHAPE
//! but not its content: binance prices spot off `commissionRates` on `/api/v3/account`, and aster's
//! fork of that endpoint does not carry the object (measured live — the body's only fee-shaped field
//! is a tier INDEX), so the lane answered `Ok(None)` forever. The slot lets aster price off its OWN
//! per-symbol `GET /api/v3/commissionRate` while binance keeps `None` and the verbatim account-body
//! arm. Not every path slot is a renamed route — this one encodes a BODY divergence, and
//! `crates/bridges/aster/tests/recon_fee_lane_routing.rs` records both venues' actual request lists
//! so the shared routing cannot quietly collapse onto one of them. Aster's PERP fee lane stays inert
//! (`perp_commission_rate: None`) — the parser is shared and ready, the wiring deliberately out of
//! scope (see `crates/bridges/aster/src/recon_client.rs`'s `ASTER_RECON`).
//!
//! **Rung 6 — the kline REST history.** `klines`: the `/klines` fetch + JSON→`Bar` map (the pure
//! `parse_klines`/`klines_url`, the un-throttled warmup seed, and the paged, weight-header-aware
//! range backfill) was a near-byte-identical copy in both venues' `data.rs`. The two real deltas —
//! host resolution (Binance `&'static str` consts vs Aster's `urls::urls_for(env)`) and the venue
//! label + rate-limit budget — are passed in: the caller resolves the `base` (host+path) and hands
//! it in, and the label+budget ride a [`klines::KlineSpec`] ([`klines::KlineRateLimit`] + the
//! `venue`). Same discipline as rungs 2–5. The spec also carries an optional
//! [`klines::KlineSpec::exchange_info_url`]: where present, the paged backfill DISCOVERS the venue's
//! published `REQUEST_WEIGHT` budget once per call and paces against it instead of the hand-measured
//! `page_delay`. That URL is a `Cow`, so it is resolved by the venue face exactly like `base` is —
//! BORROWED from a const where the host is fixed (Binance's two) and OWNED where it is composed per
//! `Environment` (Aster's four). The rung refuses a URL that is not on the host it is paging
//! ([`klines::discovery_url`]), because a published budget belongs to a HOST. Each venue keeps its OWN
//! `data.rs` wrappers (its host resolution, its public `fetch_klines_*`/`parse_klines` surface, and
//! the WIRING of its rate-limit budget) and its own host-resolution tests. The budget NUMBERS are no
//! longer a per-venue `const` in either crate: they live one layer down in
//! `vike_model::venue_rate_limits`, keyed `(venue, market)` and rowed beside every other venue's,
//! because a venue's published ceiling is a fact about the VENUE rather than a constant of whichever
//! crate happens to read it.
//!
//! **Deliberately NOT shared: `ratelimit`.** The two venues' gate CONSTRUCTION is near-identical,
//! but the only thing that actually differs — the budget numbers — is per-venue regardless (Binance
//! meters `ORDERS` per 10s, Aster per MINUTE; even their equal admitted `90` is a coincidence of
//! different derivations, not a shared value). Hoisting the shape would have cost MORE code than it
//! removed: a budgets struct is bigger than the three one-line constructors it would dedup. Measured
//! and rejected on those grounds — the two `ratelimit.rs` files stay independent on purpose. Do not
//! "finish the job" here without re-measuring. (The numbers themselves ARE now shared — as ROWS in
//! the `vike_model::venue_rate_limits` table, which is the opposite move: one table of per-venue
//! facts, not one shared constructor.)
//!
//! **Scope discipline.** `venue`/`display`/hosts are caller-supplied at every site, which is
//! exactly what makes the sharing safe: nothing here knows which venue it is serving, and nothing
//! here picks a host. The signed REST methods, the `run_*` exec drivers, recon, and the rate gates
//! stay per-venue by design (the venue-agnostic exec dispatch loop — rung 4b — and the pure order
//! mappers — rung 4c — are shared; the signed methods that call them are not). Note that Binance's OWN spot user-data path
//! ([`crate::user_data`]) is a WS-API subscribe/ack session — a genuinely different mechanism, not
//! a listenKey pump — and is deliberately NOT folded into rung 4a.
//!
//! Each venue keeps its OWN fixture/scripted tests over its OWN public wrapper, so this shared code
//! is proven twice — once against Binance's golden frames, once against Aster's.

// The rungs split by PLANE exactly as the two venue faces above them do (ruling 8): rungs 2/3/6 —
// the market-data stack, the public catalog parse and the kline REST history — are the FEEDS half
// and stay ungated, while rungs 1/4/5 (the pure exec mappers, the listenKey pump, the exec
// dispatch loop, the order mappers and the reconcile client) ride the crate's default-on `exec`
// feature. Aster forwards its own `exec` onto this crate's, so a feeds-only aster build compiles
// the feeds rungs here and nothing else.
pub mod catalog;
pub mod depth;
#[cfg(feature = "exec")]
pub mod event_mapper;
#[cfg(feature = "exec")]
pub mod exec_loop;
#[cfg(feature = "exec")]
pub mod filters_rec;
#[cfg(feature = "exec")]
pub mod history;
pub mod klines;
#[cfg(feature = "exec")]
pub mod listenkey;
pub mod market_feed;
#[cfg(feature = "exec")]
pub mod order_map;
#[cfg(feature = "exec")]
pub mod perp_mapper;
#[cfg(feature = "exec")]
pub mod recon;
pub mod trades;

/// The resolved public-endpoint hosts one venue's market-data stack reads, as plain `&'static str`
/// — the ONE real delta between the two venues' otherwise byte-identical feeds (rung 2).
///
/// Deliberately a VALUE, not a resolver: whoever builds it has already decided which network it
/// names. Binance builds it once as a `const` ([`crate::market_feed::BINANCE_URLS`]) and is
/// therefore NOT env-resolved — its hosts stay exactly what they were before this module existed.
/// Aster builds it per-`Environment` from `urls::urls_for(env)` (its testnet/mainnet threading is
/// Aster's own concern, invisible here).
///
/// WS hosts are BARE (no trailing `/ws`) — the family's URL builders append the `/ws/…` or
/// `/stream?streams=…` segment — and REST hosts are bare origins with the path supplied
/// separately, because the aggTrades path itself is a genuine per-venue divergence (Binance serves
/// perp aggTrades at `/fapi/v1/…`, Aster at `/fapi/v3/…`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlTable {
    /// Spot public-stream host, bare (Binance: `wss://stream.binance.com:9443`).
    pub spot_ws: &'static str,
    /// USDⓈ-M futures public-stream host, bare (Binance: `wss://fstream.binance.com`).
    pub perp_ws: &'static str,
    /// Spot REST origin, bare (Binance: `https://api.binance.com`).
    pub spot_rest: &'static str,
    /// USDⓈ-M futures REST origin, bare (Binance: `https://fapi.binance.com`).
    pub perp_rest: &'static str,
    /// Spot aggTrades path, leading slash (both venues: `/api/v3/aggTrades`).
    pub spot_agg_trades_path: &'static str,
    /// USDⓈ-M futures aggTrades path, leading slash. **A real venue divergence, NOT unified:**
    /// Binance serves `/fapi/v1/aggTrades`, Aster `/fapi/v3/aggTrades`.
    pub perp_agg_trades_path: &'static str,
    /// Which trade lane this venue's PERP feed rides — see [`PerpTradesLane`]. Another real
    /// divergence carried as data: Binance's futures `@aggTrade` stream is DEAD and Aster's works.
    pub perp_trades_lane: PerpTradesLane,
    /// USDⓈ-M futures RAW-trades path, leading slash — only read when `perp_trades_lane` is
    /// [`PerpTradesLane::Raw`] (Binance: `/fapi/v1/trades`, keyless, ids in the `@trade` space).
    pub perp_raw_trades_path: &'static str,
}

/// Which WS stream + REST warmup a venue's PERP trade feed uses. Spot is always `@aggTrade` on both
/// venues; this is the perp-only axis.
///
/// **Measured 2026-08-02, from two hosts, 60 s windows:**
///
/// | stream | binance fstream | aster fstream |
/// |---|---|---|
/// | `btcusdt@aggTrade` | **0 frames** | 10 / 15 s ✓ |
/// | `ethusdt@aggTrade` | **0 frames** | — |
/// | `btcusdt@trade` | 770 frames ✓ | 12 / 15 s ✓ |
///
/// Binance's futures `@aggTrade` accepts the socket and then pushes nothing — no error, no close —
/// while `@trade` and `@depth@100ms` on the identical host stream normally and the futures
/// `aggTrades` REST still answers 200. So it is that one stream, not the host or the region, and it
/// is per-VENUE: Aster (a Binance-grammar fork) serves `@aggTrade` fine and keeps riding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerpTradesLane {
    /// `@aggTrade` + the `aggTrades` REST warmup. Ids are AGGREGATE ids, the space the backward
    /// backfill pages with `fromId`.
    Aggregated,
    /// `@trade` + the `trades` REST warmup. Ids are RAW trade ids — a DIFFERENT space from
    /// `aggTrades`, which is why a venue on this lane does not report an earliest-live-id (see
    /// [`super::trades::run_trades_feed`]).
    Raw,
}

impl UrlTable {
    /// The public-stream host for this instrument class (bare — callers append the path).
    pub fn ws(&self, perp: bool) -> &'static str {
        if perp { self.perp_ws } else { self.spot_ws }
    }

    /// The REST origin for this instrument class (bare — callers append the path).
    pub fn rest(&self, perp: bool) -> &'static str {
        if perp { self.perp_rest } else { self.spot_rest }
    }

    /// The aggTrades path for this instrument class (see [`UrlTable::perp_agg_trades_path`] — the
    /// perp arm genuinely differs between the two venues).
    pub fn agg_trades_path(&self, perp: bool) -> &'static str {
        if perp { self.perp_agg_trades_path } else { self.spot_agg_trades_path }
    }
}

/// Everything the shared market-data stack needs to know about the venue it is serving this call.
/// `Copy` + all-`&'static str` so a feed thread carries it by value with no allocation.
///
/// Deliberately NOT carrying a `tracing` target: the macros bake `target:` into a `static
/// Metadata`, so it must be a const expression and cannot be a runtime field. The shared code
/// therefore logs under its own module path and carries [`FamilySpec::venue`] as a structured
/// field instead — which stays greppable per venue and still matches a `vike_binance=…` filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilySpec {
    /// The canonical venue key stamped on every event/tick (`"binance"` / `"aster"`).
    pub venue: &'static str,
    /// Human-facing venue name for GUI status strings (`"Binance"` / `"Aster"`).
    pub display: &'static str,
    /// Resolved public endpoints (see [`UrlTable`]).
    pub urls: UrlTable,
}
