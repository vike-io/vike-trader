//! Shared Binance-wire-grammar core reused by vike-aster — NOT a venue. Never registered as a
//! CatalogProvider or started as an ExecutionClient.
//!
//! Aster's API is a near-verbatim Binance fork (same event names, same field letters, same
//! `exchangeInfo` filters, same listenKey user-data transport, same public stream names and
//! depth-sync rule), so its wire code was byte-for-byte copied from Binance's. This module is the
//! ONE home for that shared grammar; the two venues' own modules are thin re-exports/callers over
//! it, each supplying its own venue string and its own host table.
//!
//! **Rung 1 (F0) — the pure mappers.** `event_mapper`/`perp_mapper`/`history`/`filters_rec`:
//! serde_json in, `vike_model` events out, `venue` a caller-supplied parameter at every site.
//! `spot_fee` is their one shared rule for a spot fill's fee asset and a BUY's quantity net of it (the
//! executionReport mapper, the history replay and the reconcile fill parser all call it). ⚠ It is the one
//! rung-1 module that can reach a socket: a fee in a third asset (BNB) is priced through a rate source the
//! VENUE registers (`execution::spot_fee::register_rates`, decision 0215), asked only on that cold path; with nothing
//! registered, as in every mapper test, it is pure.
//!
//! **Rung 2 (F8/F9/F11/F17) — the market-data stack.** `depth` (the U/u depth-sync protocol + the
//! HFT tick pump), `trades` (the aggTrade decode + the WS-buffer→REST-warmup→splice startup dance +
//! backward-paging backfill), and `market_feed` (the live kline `DataClient` feed body + the DOM
//! depth lane). Unlike rung 1 these touch the network, so the ONE real per-venue delta —
//! host/URL resolution — is passed in as a [`UrlTable`] value inside a [`FamilySpec`]: Binance
//! builds its table from its `venue_url` rows ([`crate::feed::BinanceTables`]), Aster maps
//! its own rows (`urls::AsterUrls`) into the same shape. Nothing here decides which network it is on.
//!
//! **Rung 3 (F14) — the public catalog.** `catalog`: the `exchangeInfo` spot/perp parse both
//! venues' `CatalogProvider`s delegate their `list_instruments` body to. Same discipline as rung 2
//! — the per-venue delta (the two `exchangeInfo` URLs) is passed in as plain `&str`, so Binance's
//! stay `const`s and Aster's stay its own mainnet-always resolution, and nothing here became
//! env-resolved. `CatalogProvider` itself is deliberately NOT implemented here: each venue owns its
//! struct and its `venue()`/`mode()`.
//!
//! **Rung 4a (F13) — the listenKey user-data pump.** `listenkey`: mint the key over REST → connect
//! `<websocket_base>[/ws]/<key>` → drive bridge-core's venue-neutral reliability loop, plus the resync
//! supervisor wiring. There were THREE copies of this one shape (Binance perp, Aster perp, Aster
//! spot). The per-venue delta is the listenKey call's AUTH — Binance's `X-MBX-APIKEY` header vs
//! Aster's EIP-712-signed query — passed in as a [`execution::user_data::ListenKeyAuthentication`] impl, so each venue
//! keeps only its auth. It lives here rather than in vike-bridge-core deliberately: listenKey is
//! Binance wire grammar and all three copies are family venues, so hoisting it into the neutral
//! core would have broken bridge-core's venue-neutral charter. Hosts/endpoint paths stay
//! caller-supplied, same as rungs 2–3.
//!
//! **Rung 4b — the exec dispatch loop.** `exec_loop`: `split_symbol` (the `.P`-suffix split) and
//! `run_loop` (the command→REST→ingest pump + gap-sentinel) were byte-identical copies in both
//! venues' `execution/mod.rs` (the only textual diffs were doc comments). `run_loop` — generic over the REST
//! surface (`R: VenueRest`) and a resync closure, with ZERO venue-local dependency — has since been
//! hoisted further down to [`vike_bridge_core::exec_actor::run_loop`]: deribit/bybit/okx carried the
//! same verbatim copy, so it is not even family-specific, and all four venues now call the ONE
//! bridge-core copy. `exec_loop` keeps `split_symbol` (family wire grammar). The `run_spot`/`run_perp`
//! drivers joined the family in rung 4f below; each venue keeps its own `run` and its actor.
//! The pure spot/perp response mappers (`json_id`/`parse_symbol_properties`/the order-param
//! builders) were a LARGER but SEPARATE dedup candidate, an `exec_loop`-sibling shared next as
//! rung 4c ([`execution::order_map`]).
//!
//! **Rung 4c — the pure order mappers.** `order_map`: the pure, signing-/path-independent
//! slices of the signed REST clients that were byte-identical copies in both venues' `execution/rest/spot.rs`/
//! `execution/rest/perp.rs`. `json_id` (numeric-id coercion; four private copies), `parse_symbol_properties` (the
//! spot `/exchangeInfo`→`SymbolProperties` parse, `pub`-re-exported by each `spot`) and the spot/perp
//! `build_*_order_params` payload shapers (each venue's `build_order_params` METHOD is now a one-line
//! delegation, `symbol`/`properties` passed in). The param ORDER is load-bearing (r6 fixtures pin the
//! exact byte output), identical between the two venues; `format_to_step_f` stays the pinned Decimal
//! wire site. Left per-venue and NOT folded (each names a diverging path or a crate-local type r6
//! constructs by name): the crate-local `PerpInstrument` + `parse_*_perp_instruments` and the
//! submit-event extraction.
//!
//! **Rung 4d — the path-only REST reads.** `rest_reads`: `get_all_orders` (the audit-A3 resync's
//! `allOrders` read — four copies, spot and perp of both venues) and `server_time_offset` (the `/time`
//! skew read — two) were byte-identical across the two venues apart from the endpoint literal. Each is now one body taking
//! the transport, the signer, the host, the path and the symbol, and each client's method is a
//! one-line delegation that supplies its own path — the same "the delta is a parameter" discipline as
//! the rungs above. `resolve_ambiguous_submit` is deliberately NOT folded in: see that module's doc
//! for why four identical-looking copies are not one function. Its callee, the audit-T1
//! `query_order_orderid` re-query (also four copies), IS folded in as a third read: the one thing
//! the two venues differ in, the `origClientOrderId` they send, is a parameter. What the answer BECOMES
//! (read by status) is `confirm`; the guard rails of the active confirm are `vike_bridge_core::confirm`.
//! The perp amend (`modify_perp_order`, `PUT` of each venue's `PATH_ORDER`) joined it the same way:
//! the path, the `origClientOrderId` and the reply field holding the amend's time are parameters
//! (`execution::rest_reads::PerpModifySpec`); binance's refusal of an algo-order amend stays in its own client.
//!
//! **Rung 4e — the signed REST bodies** (the owner's ruling of 2026-11-13, "both parts, behaviour as
//! now"). `signed_rest`: the submit, the native batch run, the ambiguous-submit re-query, the active
//! confirm, the cancel, the resync reads, the leverage write and the perp amend were the same bodies on
//! the four clients (binance's and aster's spot and perp) over different paths and binance's broker
//! link id. Each client keeps its struct and implements [`execution::signed_rest::FamilySignedRest`]: its parts,
//! its [`execution::signed_rest::FamilyRestPaths`] and its venue's facts around the wire — the bridge FLOORS
//! (binance's TIF/trigger/kind gates on both lanes; aster's perp trigger gate; none on aster spot), a
//! refusal's cause and words (binance's `post_only_cause` and perp reject words; aster's venue words),
//! the refusal's log line under the client's own target, and binance's Algo Service route and
//! two-endpoint lookup (decision 0186, still `crate::execution::rest::algo`'s).
//!
//! **Rung 4f — the exec plane.** `execution_plane`: the confirm, the foreign-symbol factories, the
//! startup grid, the `run_spot`/`run_perp` drivers and the audit-A3 replay, over a venue's
//! [`execution::FamilyExecutionSpec`] (its hosts, attribution, client builds, user-data spawners,
//! perp start writes, binance's COIN-M fence and preparations, and its log target: the plane's lines go
//! through `family_execution_log!` with the venue's literal target, so `vike_aster::execution` stays
//! `vike_aster::execution`). Both rungs were pinned on the code before the move
//! (`crates/bridges/aster/tests/offline/family_wire_pins.rs`, `crates/bridges/binance/tests/offline/family_wire_pins.rs`).//!
//! **Rung 5 — the reconcile client.** `recon`: the venue-facing `ReconcileClient` report seam (spot +
//! perp order/fill/position/balance/fee reports) was a byte-identical copy in both venues'
//! `reconcile/mod.rs` — the pure `parse_*` body parsers AND the fetch/dispatch client, differing only
//! in the endpoint PATHS (Binance's `/fapi/v1|v2` split vs Aster's single `/fapi/v3`) and the venue
//! label. Both now pass into ONE [`recon::ReconcileClientBinanceFamily`], with the per-venue delta supplied as a
//! [`recon::ReconcileSpec`] (a [`recon::ReconcilePaths`] table + the `venue` string), same discipline as
//! rungs 2–4. Each venue keeps a thin named wrapper (`ReconcileClientBinance`/`ReconcileClientAster`) pinned
//! to its spec, plus 1-arg `parse_*` re-exports that inject its own `venue`, so the shared parsers
//! stay proven twice against each venue's fixtures. Healed a real drift here: aster's copy predated
//! binance's #416 fee lane, so aster reconcile never refreshed live fees.
//!
//! ⚠ That heal took TWO steps, and the second is why [`recon::ReconcilePaths`] carries a
//! `spot_commission_rate` slot. Routing aster through the shared client restored the lane's SHAPE
//! but not its content: binance prices spot off `commissionRates` on `/api/v3/account`, and aster's
//! fork of that endpoint does not carry the object (measured live — the body's only fee-shaped field
//! is a tier INDEX), so the lane answered `Ok(None)` forever. The slot lets aster price off its OWN
//! per-symbol `GET /api/v3/commissionRate` while binance keeps `None` and the verbatim account-body
//! arm. Not every path slot is a renamed route — this one encodes a BODY divergence, and
//! `crates/bridges/aster/tests/offline/reconcile_fee_lane_routing.rs` records both venues' actual request lists
//! so the shared routing cannot quietly collapse onto one of them. Aster's PERP fee lane stays inert
//! (`perp_commission_rate: None`) — the parser is shared and ready, the wiring deliberately out of
//! scope (see `crates/bridges/aster/src/reconcile.rs`'s `ASTER_RECONCILE`).
//!
//! **Rung 6 — the kline REST history.** `klines`: the `/klines` fetch + JSON→`Bar` map (the pure
//! `parse_klines`/`klines_url`, the un-throttled warmup seed, and the paged, weight-header-aware
//! range backfill) was a near-byte-identical copy in both venues' `history.rs`. The two real deltas —
//! host resolution (each venue's `venue_url` rows, looked up by the venue face) and the venue
//! label + rate-limit budget — are passed in: the caller resolves the `base` (host+path) and hands
//! it in, and the label+budget ride a [`history::klines::KlineSpec`] ([`history::klines::KlineRateLimit`] + the
//! `venue`). Same discipline as rungs 2–5. The spec also carries an optional
//! [`history::klines::KlineSpec::exchange_info_url`]: where present, the paged backfill DISCOVERS the venue's
//! published `REQUEST_WEIGHT` budget once per call and paces against it instead of the hand-measured
//! `page_delay`. That URL is a `Cow`, so it is resolved by the venue face exactly like `base` is —
//! BORROWED from a const where the host is fixed (Binance's two) and OWNED where it is composed per
//! tier (`AccountTier`, Aster's four). The rung refuses a URL that is not on the host it is paging
//! ([`history::klines::discovery_url`]), because a published budget belongs to a HOST. Each venue keeps its OWN
//! `history.rs` wrappers (its host resolution, its public `fetch_klines_*`/`parse_klines` surface, and
//! the WIRING of its rate-limit budget) and its own host-resolution tests. The budget NUMBERS are no
//! longer a per-venue `const` in either crate: they live one layer down in
//! `vike_venues::venue_rate_limits`, keyed `(venue, market)` and rowed beside every other venue's,
//! because a venue's published ceiling is a fact about the VENUE rather than a constant of whichever
//! crate happens to read it.
//!
//! **Four family facts with no rung.** [`execution::order_word`]: each order `status` word has one reading, which
//! every reader of a venue order row matches exhaustively. [`transport`]: one clock offset per client, for
//! every signer and auth frame of both venues. [`signer`]: the HMAC request signer both venues sign
//! with. [`rate_discovery`]: the `REQUEST_WEIGHT` budget read out of an `exchangeInfo` body. The last two
//! were one-venue code in `vike-bridge-core` until they moved here (a shared home keeps only the
//! venue-neutral half: the `Signer` seam and the `Pacer`'s `WeightBudget`).
//!
//! **Deliberately NOT shared: `ratelimit`.** The two venues' gate CONSTRUCTION is near-identical,
//! but the only thing that actually differs — the budget numbers — is per-venue regardless (Binance
//! meters `ORDERS` per 10s, Aster per MINUTE; even their equal admitted `90` is a coincidence of
//! different derivations, not a shared value). Hoisting the shape would have cost MORE code than it
//! removed: a budgets struct is bigger than the three one-line constructors it would dedup. Measured
//! and rejected on those grounds — the two `transport/ratelimit.rs` files stay independent on purpose. Do not
//! "finish the job" here without re-measuring. (The numbers themselves ARE now shared — as ROWS in
//! the `vike_venues::venue_rate_limits` table, which is the opposite move: one table of per-venue
//! facts, not one shared constructor.)
//!
//! **Scope discipline.** `venue`/`display`/hosts are caller-supplied at every site, which is
//! exactly what makes the sharing safe: nothing here knows which venue it is serving, and nothing
//! here picks a host. Since rungs 4e/4f the signed REST bodies and the exec drivers are shared too,
//! every venue fact a trait item of the venue's own (`FamilySignedRest`, `FamilyExecutionSpec`); the
//! signer, the recon spec, the rate gates, each venue's actor and its one-venue verbs stay per-venue
//! by design. Note that Binance's OWN spot user-data path
//! ([`crate::execution::user_data::spot`]) is a WS-API subscribe/ack session — a genuinely different mechanism, not
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
pub mod rate_discovery;
#[cfg(feature = "execution")]
pub mod recon;
#[cfg(feature = "execution")]
pub mod signer;

#[cfg(feature = "execution")]
pub mod execution;
pub mod feed;
pub mod history;
#[cfg(feature = "execution")]
pub mod instrument;
#[cfg(feature = "execution")]
pub mod transport;

/// The CORE/series label of the USDⓈ-M perpetual the venue itself names `exchange_symbol`:
/// `"BTCUSDT"` → `"BTCUSDT.P"` — the exchange symbol with [`vike_venues::PERP_SUFFIX`] appended.
/// The inverse of `execution::dispatch::split_symbol`, which strips that suffix for the wire.
///
/// **One function, because three sites must spell a perp identically.** The picker's catalog id
/// ([`catalog::parse_perp`]) is the string a perp engine on and the Trade window ORDERS
/// on, and an engine folds a fill only when the fill carries that exact string
/// (`vike_execution::Engine::accepts_symbol` is string equality). So the perp user-data
/// mapper's fill and liquidation labels (`perp_mapper`) and the perp reconcile reports (`recon`)
/// must produce the same spelling the catalog does. Until 2026-10-03 those two labelled the BARE
/// frame/row symbol, and every live aster and binance perp fill was dropped from the position:
/// the order reached FILLED while position, realized PnL and commission stayed where they were.
///
/// It labels the symbol the venue NAMED, never the one a caller has started: a user-data stream is
/// account-wide, so a foreign `ETHUSDT` execution becomes `ETHUSDT.P` and lands on its own engine.
///
/// An EMPTY exchange symbol stays empty — a frame or row that named no instrument is not handed
/// one, and a bare `.P` would look like a label.
///
/// ⚠ "PERPETUAL" names what the CATALOG feeds it, not what the function checks: it suffixes any
/// symbol it is handed. The catalog admits `contractType == "PERPETUAL"` only, but the fapi user-data
/// stream is account-wide, so an execution of a USDⓈ-M DELIVERY contract (`BTCUSDT_250926`) is
/// labelled `BTCUSDT_250926.P` too. That is harmless rather than wrong: it folds only into an engine
/// started on that exact spelling, which the picker never offers, and otherwise lands on no engine —
/// the same fate the bare label gave it.
pub fn perp_series_symbol(exchange_symbol: &str) -> String {
    if exchange_symbol.is_empty() {
        return String::new();
    }
    format!("{exchange_symbol}{}", vike_venues::PERP_SUFFIX)
}

/// The fill dedup key of an execution whose venue trade id is `t`, on the series `fill_symbol`, for
/// an engine MOUNTED on `mounted` (series labels; the lane suffix `.P` is not part of the comparison, as
/// a spot symbol never arrives on the perp stream).
///
/// ⚠ `t` is a per-SYMBOL sequence on this grammar (binance and aster alike), and the engine's fill
/// ledger keys on the bare string (`vike_execution::Account`'s "Fill dedup"), so once one account trades
/// two symbols two genuine fills can share a `t` and the second would be dropped as a duplicate. A
/// fill on the MOUNTED symbol keeps the bare `t` (byte-identical: the resync replay, the reconcile
/// fetch and every restored ledger spell it that way); a fill on any other symbol is qualified
/// `SYMBOL:t`. Three readers spell a foreign symbol's fill, and all through THIS function: the
/// user-data stream's mappers, the reconcile client's fill read of an ADMITTED symbol
/// (`recon::ReconcileClientBinanceFamily`) and the history replay of the resync and the gap-sentinel
/// (`execution::resync::map_history_of`, `execution::resync::map_perp_history_of`), so nothing ever spells a foreign fill
/// bare (aster shares all of it). `Err` for an empty `t`, as [`vike_model::events::TradeId::new`].
#[cfg(feature = "execution")]
pub(crate) fn fill_trade_id(
    t: &str,
    fill_symbol: &str,
    mounted: &str,
) -> Result<vike_model::events::TradeId, vike_model::events::TradeIdError> {
    let same = |a: &str, b: &str| {
        vike_venues::split_perp(a).0.eq_ignore_ascii_case(vike_venues::split_perp(b).0)
    };
    if t.is_empty() || fill_symbol.is_empty() || same(fill_symbol, mounted) {
        return vike_model::events::TradeId::new(t);
    }
    vike_model::events::TradeId::new(format!("{fill_symbol}:{t}"))
}

/// The grammar's `positionSide` (REST rows) / `ps` (user-data frames) word as a typed
/// [`vike_model::events::PositionSide`] — the ONE place either venue reads it. `BOTH`/`LONG`/`SHORT`
/// map verbatim; an ABSENT or EMPTY field is the one-way `Both` (spot rows and TRADE_LITE frames
/// carry none). Any other word is outside the grammar's documented three: it is WARNED and read as
/// `Both`, the one-way bucket whose signed qty still carries the direction — never dropped, never
/// silently guessed.
pub fn position_side_of(raw: Option<&str>, venue: &str) -> vike_model::events::PositionSide {
    use vike_model::events::PositionSide;
    match raw {
        None | Some("") | Some("BOTH") => PositionSide::Both,
        Some("LONG") => PositionSide::Long,
        Some("SHORT") => PositionSide::Short,
        Some(other) => {
            tracing::warn!(
                venue,
                position_side = other,
                "unknown positionSide word — reading it as the one-way BOTH bucket"
            );
            PositionSide::Both
        }
    }
}

/// **A `venue_url` row's address as the `&'static str` a [`UrlTable`] holds**: leaked ONCE per
/// distinct text and handed back from a process-wide set ever after.
///
/// The rows are read once at the process's start (`vike_venues::venue_urls::VenueUrls`), so the set
/// holds one entry per address a binance or aster table was ever built from: a handful of short
/// strings for the life of the process, however many feeds are built over them. Kept so that
/// [`UrlTable`] and [`FamilySpec`] stay `Copy` values a feed thread carries with no allocation.
pub fn static_url(url: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock, PoisonError};
    static SET: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = SET
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(&known) = set.get(url) {
        return known;
    }
    let leaked: &'static str = Box::leak(url.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// The resolved public-endpoint hosts one venue's market-data stack reads, as plain `&'static str`
/// — the ONE real delta between the two venues' otherwise byte-identical feeds (rung 2).
///
/// Deliberately a VALUE, not a resolver: whoever builds it has already decided which network it
/// names. Both venues build it from their `venue_url` rows (`vike_venues::venue_urls::VenueUrls::url`,
/// through [`static_url`]): binance's at `live` `data` ([`crate::feed::BinanceTables`]), aster's
/// at the tier its caller names (`urls::AsterUrls`; its testnet/mainnet threading is Aster's own
/// concern, invisible here).
///
/// WS hosts are BARE (no trailing `/ws`) — the family's URL builders append the `/ws/…` or
/// `/stream?streams=…` segment — and REST hosts are bare origins with the path supplied
/// separately, because the aggTrades path itself is a genuine per-venue divergence (Binance serves
/// perp aggTrades at `/fapi/v1/…`, Aster at `/fapi/v3/…`).
///
/// **The futures host is routed by stream CLASS** (Binance's March 2026 websocket upgrade, decision
/// 0251): a stream is dialled at `<perp_websocket><path of its class>/ws/<stream>`, and the legacy path
/// (no class segment) delivers the public class ONLY — kline, markPrice and aggTrade answer 200 and
/// then push nothing. [`UrlTable::market_websocket`] and [`UrlTable::public_websocket`] are the two doors; there
/// is no class-blind accessor, so a new stream has to name its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlTable {
    /// Spot public-stream host, bare (Binance: `wss://stream.binance.com:9443`).
    pub spot_websocket: &'static str,
    /// USDⓈ-M futures public-stream host, bare (Binance: `wss://fstream.binance.com`).
    pub perp_websocket: &'static str,
    /// Path segment between [`UrlTable::perp_websocket`] and `/ws/` for the futures MARKET class — kline,
    /// markPrice, aggTrade, ticker, forceOrder (Binance: `/market`). Empty on a venue that serves
    /// every stream on one path (Aster, measured 2026-10-31: its `/ws`, `/market` and `/public`
    /// all answer every stream, and it dials `/ws`).
    pub perp_market_path: &'static str,
    /// Path segment for the futures PUBLIC class — bookTicker, diff/partial depth and the `trade`
    /// stream (Binance: `/public`; empty on Aster, as above).
    pub perp_public_path: &'static str,
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
///
/// **Why it was dead (found 2026-10-31, decision 0251):** `@aggTrade` belongs to the futures MARKET
/// class, which Binance moved to `/market/ws/…` and stopped serving on the legacy path this lane
/// dialled; `@trade` is served on the PUBLIC class, which the legacy path still carries. The raw
/// lane stays (its ids are the ones the recorded tape already holds) and now dials `/public`;
/// `@aggTrade` answers again on `/market` and is what the `Aggregated` lane would dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerpTradesLane {
    /// `@aggTrade` + the `aggTrades` REST warmup. Ids are AGGREGATE ids, the space the backward
    /// backfill pages with `fromId`.
    Aggregated,
    /// `@trade` + the `trades` REST warmup. Ids are RAW trade ids — a DIFFERENT space from
    /// `aggTrades`, which is why a venue on this lane does not report an earliest-live-id (see
    /// [`feed::tick::trades::run_trades_feed`]).
    Raw,
}

impl UrlTable {
    /// The stream base for the MARKET class (kline, markPrice, aggTrade): callers append `/ws/…`.
    /// Spot has no classes and answers on its one host.
    pub fn market_websocket(&self, perp: bool) -> String {
        if perp {
            format!("{}{}", self.perp_websocket, self.perp_market_path)
        } else {
            self.spot_websocket.into()
        }
    }

    /// The stream base for the PUBLIC class (bookTicker, depth, `trade`): callers append
    /// `/ws/…` or `/stream?streams=…`. Spot has no classes and answers on its one host.
    pub fn public_websocket(&self, perp: bool) -> String {
        if perp {
            format!("{}{}", self.perp_websocket, self.perp_public_path)
        } else {
            self.spot_websocket.into()
        }
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
    /// The text the kline, mark and trade lanes send at their pump row's `keepalive_every`
    /// (`vike_bridge_core::websocket_session::spec`'s `row`), `None` where the venue's row declares none: binance's
    /// server ping keeps its sockets up, while aster's perp host cuts a client silent for 240 s
    /// (`vike_aster::feed::KEEPALIVE`). The pump debug-asserts the two agree.
    pub market_keepalive: Option<&'static str>,
}

#[cfg(all(test, feature = "execution"))]
#[path = "fill_trade_id_tests.rs"]
mod fill_trade_id_tests;
