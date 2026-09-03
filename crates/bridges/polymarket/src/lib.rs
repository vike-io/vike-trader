//! Polymarket CLOB venue adapter (prediction markets). Native, pure-Rust — reference spec is the
//! official `polymarket-client-sdk-v2` (NOT a runtime dep). Behind TWO crate-local cargo features
//! since the split-plane Phase-5 seam: `feeds` (the keyless market-data plane) and `polymarket`
//! (the full venue, implying `feeds`).
//!
//! DATA half: unauthenticated CLOB book/midpoint reads (the shared `RestTransport` seam) and the
//! market-channel WS book feed (`ws`). EXEC half: EIP-712 order signing (`order`/`l1`, over the
//! shared `vike_bridge_core::eip712` — CLOB **V2** domain/struct), L2-HMAC submit/cancel
//! (`auth`/`exec`), and the authenticated user-channel WS pump (`user_data`/`user_ws`). Symbols
//! are ERC-1155 outcome `token_id`s.
//!
//! Moved into crates/bridges/polymarket (crate-reorg Phase 3, PR I). The crate-level gate below is
//! `#![cfg(feature = "feeds")]` — the whole tree still vanishes from a default (no-feature) build
//! (an empty crate, exactly as when the gate spelled `polymarket`) — and the EXEC plane is a
//! module-level `#[cfg(feature = "polymarket")]` on top of it (split-plane Phase 5): unlike
//! vike-fxcm's `fxcm` feature (which only gates `build.rs`'s native compile attempt — that module
//! tree always compiles, stubbing internally via `#[cfg(not(fcsdk))]`), the exec tree names
//! `vike_bridge_core::eip712` DIRECTLY at every call site (the ONE shared EIP-712 primitive,
//! itself behind bridge-core's `eip712` feature which the `polymarket` feature enables) — a
//! module that does not exist without the feature, and whose former byte-identical local copy in
//! this crate is deleted, so do not re-create one. The exec tree therefore has no way to compile
//! a stub without it, while the feed tree deliberately names no signing type at all and compiles
//! under `--features feeds` alone.
//!
//! ## Live-vs-parked map (verified against every consumer, 2026-07-28)
//!
//! **LIVE — on a production path today** (composed by `mount`/`recon_client` and mounted through
//! vike-mount, or consumed directly by vike-app / vike-run / vike-tradehub / vike-backfill):
//! exec half `client` / `exec` / `user_data` / `user_ws` / `order` / `l1` / `auth` / `config` /
//! `registry` / `ratelimit` / `rate_budget` / `fill_tracker` / `history` / `neg_risk_lookup` /
//! `mount` / `recon_client`; data half `market_feed` / `ws` / `data` / `rtds` / `raw_tap` /
//! `instruments` / `taker_hold` / `filters_rec` / `gamma` / `catalog` / `neg_risk_set` /
//! `universe` / `rewards` / `toxicity_agg` / `wallet_class`.
//!
//! `rate_budget` is LIVE as of the rate-budget wiring: `exec` mirrors both token buckets on
//! every submit/cancel and reconciles them to the venue's `Poly-RateLimit-*` headers. The RESERVE
//! INVARIANT is now DRIVEN too: [`gate_cancel_shared`] runs it from [`client`]'s
//! `ExecCommand::Cancel` arm — the door every core cancel comes through — for a
//! `vike_exec::CancelIntent::Routine` cancel and for nothing else, which became possible once the
//! shared `ExecutionClient` seam carried an intent (`cancel_with_intent`). The BULK
//! targeted-vs-`cancel-all` choice is DRIVEN too, and needed a second shared-seam change to get
//! there: [`client`]'s `ExecCommand::CancelBatch` arm hands a whole batch to [`cancel_orders`],
//! which is reachable at all only because `ExecActor` stopped shredding a batch into `n` singles
//! before any venue code ran. See that function's doc for the two declared residuals.
//!
//! **PARKED — built + fixture-tested, NO production caller today** (deliberately held, each
//! waiting on its enablement decision — not dead code): the whole [`settlement`] cluster (opt-in
//! post-trade money/book movement; its module doc is the cluster contract), `discovery`
//! (rolling-window market discovery), `convert_arb` (neg-risk convert-arb detector), `scoring`
//! (order-scoring reads), `egress`'s expected-egress PROBE half (the module's proxy/agent/
//! `get_json` plumbing is LIVE — re-homed there from `exec` for the feeds/exec seam; every REST
//! call and WS dial rides it, and its `check_order_placement_geo` half is LIVE too, since the exec
//! mount refuses on a `blocked` answer), and `heartbeat` (server-side dead-man
//! switch). TWO modules LEFT this list: `tick_regime` — the live mount now builds one and the exec
//! thread prices submits on it — and `rate_budget`, whose client-side mirror the exec actor now
//! drives end-to-end (submit gate, cancel-token reserve, targeted-over-`cancel-all`, header
//! reconcile), not just [`rate_budget::parse_headers`]. See each module's doc and [`mount`].

// The feeds/exec seam (split-plane Phase-5 hardening): the crate-level gate is now the `feeds`
// feature — the keyless market-data plane, which everything else in this crate builds on — and
// every module that signs (EIP-712 / L2-HMAC), pumps private user-data, mounts, reconciles or
// settles sits behind the additional `polymarket` feature (which implies `feeds`), so a
// `--features feeds` consumer links the feed surface only and vike-bridge-core's `eip712` (the
// whole k256/keccak stack) never enters its binary. A no-feature build stays an EMPTY crate,
// exactly as before the split. The `bridges-feeds` arm of scripts/ci_feature_suite.sh is the
// gate, including the structural no-signer-crate cargo-tree check.
#![cfg(feature = "feeds")]

#[cfg(feature = "polymarket")]
mod auth;
pub mod catalog;
#[cfg(feature = "polymarket")]
mod client;
mod config;
#[cfg(feature = "polymarket")]
pub mod convert_arb;
mod data;
pub mod discovery;
mod egress;
#[cfg(feature = "polymarket")]
mod exec;
#[cfg(feature = "polymarket")]
pub mod fill_tracker;
pub mod filters_rec;
pub mod gamma;
#[cfg(feature = "polymarket")]
pub mod heartbeat;
#[cfg(feature = "polymarket")]
mod history;
mod instruments;
#[cfg(feature = "polymarket")]
mod l1;
mod market_feed;
#[cfg(feature = "polymarket")]
pub mod mount;
pub mod neg_risk_lookup;
pub mod neg_risk_set;
#[cfg(feature = "polymarket")]
mod order;
// The bounded, expiring staging area for a user-channel event whose CLOB order id `registry` cannot
// re-key YET (the ack race) — the fix for an executed fill silently vanishing at `user_ws`'s three
// re-key sites. Public because `PendingStats` is the observable half of "a silent drop must not
// survive in any form": it is read off the shared registry (`PolymarketRegistry::pending_stats`).
#[cfg(feature = "polymarket")]
pub mod pending_events;
#[cfg(feature = "polymarket")]
pub mod rate_budget;
// `polymarket`-gated DELIBERATELY, unlike hl/aster's feed-plane `ratelimit` modules: this one is
// the USER-channel WS-send gate, and its only consumer is the exec-plane user-data pump.
#[cfg(feature = "polymarket")]
mod ratelimit;
pub mod raw_tap;
#[cfg(feature = "polymarket")]
pub mod recon_client;
#[cfg(feature = "polymarket")]
mod registry;
pub mod rewards;
pub mod rtds;
#[cfg(feature = "polymarket")]
pub mod scoring;
// The 9-module opt-in settlement cluster — chain / resolve / auto_redeem / redeem / redeem_confirm
// / redeem_ledger / redeem_relayer / split_merge / positions — lives under settlement/; its module
// doc states the cluster contract ONCE (opt-in env gates, default OFF, no composition-root wiring
// today). The aliases below keep every pre-move path (`vike_polymarket::chain::…`,
// `crate::redeem::…`, the flat item re-exports further down) resolving unchanged, so the directory
// is a pure `mod`-path move.
#[cfg(feature = "polymarket")]
pub mod settlement;
#[cfg(feature = "polymarket")]
pub use settlement::{
    auto_redeem, chain, positions, redeem_confirm, redeem_relayer, resolve, split_merge,
};
#[cfg(feature = "polymarket")]
pub(crate) use settlement::{redeem, redeem_ledger};
pub mod taker_hold;
pub mod tick_regime;
pub mod toxicity_agg;
pub mod universe;
#[cfg(feature = "polymarket")]
mod user_data;
#[cfg(feature = "polymarket")]
mod user_ws;
pub mod wallet_class;
mod ws;

#[cfg(feature = "polymarket")]
pub use auto_redeem::{
    auto_redeem_enabled, kill_switch_tripped, redeem_once, AutoRedeemHandle, AutoRedeemPoller,
    ProdDeps, RedeemDeps, RedeemTickReport,
};
pub use catalog::{markets_to_instruments, MarketCatalog, PolymarketCatalog};
#[cfg(feature = "polymarket")]
pub use chain::{
    chain_watch_enabled, decode_condition_resolution, decode_ctf_redemption,
    decode_neg_risk_redemption, decode_redemption, decode_token_transfer, join_settlement,
    ChainOracle, ChainResolution, ChainSettlement, ChainTick, ChainWatchHandle, ChainWatchPoller,
    ChainWatcher, PolygonRpc, RedeemVenue, Redemption, ResolutionEvent, TokenTransfer,
    CHAIN_WATCH_ENV, DEFAULT_RPC_URL, RPC_URL_ENV, TOPIC_CONDITION_RESOLUTION,
    TOPIC_CTF_PAYOUT_REDEMPTION, TOPIC_NEG_RISK_PAYOUT_REDEMPTION,
};
#[cfg(feature = "polymarket")]
pub use client::{
    PolymarketExecutionClient, PolymarketLiveConfig, UserChannelConfig,
    DEFAULT_HISTORY_LIMIT as EXEC_DEFAULT_HISTORY_LIMIT,
};
#[cfg(feature = "polymarket")]
pub use convert_arb::{
    detect as detect_convert_arb, evaluate_lock, sum_best_asks, ConvertArbConfig,
    ConvertArbOpportunity, LockKind, SizePolicy, SizedLock,
};
pub use discovery::{
    render_slug, FetchSpec, GammaSlugResolver, GammaSource, MarketFilter, PredicateFilter,
    RollingTick, RollingWindowPlanner, SearchFilter, SlugPrefixFilter, TagFilter, WindowResolver,
    WindowSpec,
};
pub use egress::{
    check_expected_egress, check_order_placement_geo, geoblock_verdict, get_json, observe_egress,
    observe_geoblock, parse_egress, parse_geoblock, proxy_url, ws_proxy, Egress, EgressCheck,
    Geoblock, GeoblockVerdict, DEFAULT_EGRESS_PROBE, GEOBLOCK_URL,
};
#[cfg(feature = "polymarket")]
// NOTE no `CancelIntent` here any more: the classification is `vike_exec::CancelIntent`, on the
// shared `ExecutionClient` seam, and this crate's byte-identical local copy was DELETED rather than
// re-exported (the workspace's no-`pub use`-shim-on-a-move rule — a second name for one fact rots).
pub use exec::{
    cancel_all_orders, cancel_body, cancel_order, cancel_order_relayer, cancel_orders, gate_cancel,
    gate_cancel_shared, get_orders, get_signed, get_trades, plan_cancels, rate_gate_enforced,
    rate_gate_would_block_count, submit_body, submit_order, submit_order_relayer, CancelBatch,
    CancelPlan, CancelScope, POLY_RATE_GATE_ENV,
};
#[cfg(feature = "polymarket")]
pub use fill_tracker::{
    FillTracker, SnapOutcome, DEFAULT_DUST_SNAP_THRESHOLD, DUST_FRACTION_OF_SUBMITTED,
    DUST_TRADE_ID_SUFFIX,
};
pub use filters_rec::{record_token_properties, record_token_tick};
pub use gamma::{
    decode_json_string_array, decode_json_string_f64_array, neg_risk_question_id,
    parse_gamma_events, parse_gamma_markets, GammaClient, GammaMarket,
};
// The OPT-IN server-side dead-man heartbeat: nothing spawns a beat unless `POLY_HEARTBEAT=1` AND
// creds are present, so a default build never posts `/v1/heartbeats` and is byte-identical.
#[cfg(feature = "polymarket")]
pub use heartbeat::{
    beat_once, heartbeat_enabled, BeatReport, HeartbeatHandle, HeartbeatPoller, HeartbeatState,
    HeartbeatTransport, ProdTransport, DEFAULT_BEAT_INTERVAL, HEARTBEAT_ENV, HEARTBEAT_PATH,
};
#[cfg(feature = "polymarket")]
pub use history::map_polymarket_history;
pub use instruments::{
    fetch_all_markets, fetch_markets_page, fetch_tick_size_direct, fetch_token_neg_risk,
    fetch_token_tick_size, fetch_token_tick_size_paged, is_neg_risk, next_cursor, parse_markets,
    parse_neg_risk, parse_tick_size, PolyMarket, PolyToken,
};
pub use market_feed::{
    run_session, run_shard_session, Feeds, MarketStream, PumpMode, PumpTiming, StreamErr,
    TokenSlot, TokenState, Watchdog, DEFAULT_TOKENS_PER_SOCKET, TOKENS_PER_SOCKET_ENV,
};
#[cfg(feature = "polymarket")]
pub use mount::{
    geoblock_action, geoblock_override_enabled, live_mount_for_account, live_mount_from_vars,
    poly_exec_enabled, poly_exec_markets, presubmit_register_enabled, GeoblockAction,
    PolymarketMount, POLY_EXEC_ENV, POLY_EXEC_MARKETS_ENV, POLY_GEOBLOCK_OVERRIDE_ENV,
    POLY_PRESUBMIT_REGISTER_ENV,
};
pub use neg_risk_lookup::{clob_neg_risk_fetch, NegRiskFetch, NegRiskSource};
pub use neg_risk_set::{NegRiskMember, NegRiskSet, SetCompleteness};
pub use universe::{
    select_universe, RankBy, SelectedMarket, UniverseDiff, UniverseManager, UniversePolicy,
};
// crate-root re-export so raw_tap.rs (and any future sibling module) can reach the venue tag via
// `crate::VENUE` without duplicating the literal.
pub(crate) use market_feed::VENUE;
#[cfg(feature = "polymarket")]
pub use positions::{
    parse_positions, payout_of, redeemable, redeemable_by, resolved_candidates, Payout, Position,
    PositionsClient, WinnerSource, DATA_API,
};
#[cfg(feature = "polymarket")]
pub use rate_budget::{
    CancelDecision, CancelStrategy, RateBudget, RateLimitSignal, SubmitDecision, Tier,
};
pub use raw_tap::{RawCaptureConfig, RawTap, RawTapHandle, RawTapOwner, TappedStream};
#[cfg(feature = "polymarket")]
pub use recon_client::{
    poly_reconcile_enabled, recon_client, recon_client_for_account, recon_client_from_vars,
    settlement_fill_report, signature_type_for_account, signature_type_from_vars,
    PolymarketReconClient, POLY_RECONCILE_ENV,
};
#[cfg(feature = "polymarket")]
pub use registry::PolymarketRegistry;
#[cfg(feature = "polymarket")]
pub use resolve::{
    ambiguous_conditions, payout_for, pm_resolve_enabled, resolved_conditions, settle_once,
    settlement_fill, settlement_key, settlement_trade_id, winning_tokens, ChainResolveDeps,
    PayoutSource, ProdResolveDeps, ResolveDeps, ResolveHandle, ResolvePoller, ResolveWatchlist,
    SettleTickReport, SettlementLedger, WatchEntry, DEFAULT_POLL_INTERVAL, LOSER_PAYOUT,
    WINNER_PAYOUT,
};
pub use rewards::RewardsConfig;
#[cfg(feature = "polymarket")]
pub use scoring::{
    fetch_order_scoring, fetch_orders_scoring, order_scoring_query, orders_scoring_query,
    parse_order_scoring, parse_orders_scoring, ORDERS_SCORING_PATH, ORDER_SCORING_PATH,
};
// The OPT-IN RTDS underlying reference-price feed: nothing constructs an `RtdsFeed`, so a build
// that never calls `RtdsFeed::start` never dials it and never emits a single sink call.
pub use rtds::{
    decode_activity_trades, decode_ref_prices, normalize_ts_ms, rtds_subscribe_message,
    rtds_subscribe_message_filtered, rtds_symbol_filter, run_rtds_activity_session,
    run_rtds_session, ActivityTrade, ActivityTradeSink, RefPrice, RtdsActivityFeed, RtdsConfig,
    RtdsFeed, TradeSide, RTDS_CRYPTO_SYMBOLS_OBSERVED, RTDS_IDLE_THRESHOLD,
    RTDS_KEEPALIVE_INTERVAL, RTDS_PING, RTDS_TYPE_SUBSCRIBE, RTDS_TYPE_TRADES, RTDS_TYPE_UPDATE,
    TOPIC_ACTIVITY, TOPIC_CRYPTO_PRICES, TOPIC_CRYPTO_PRICES_CHAINLINK, TOPIC_EQUITY_PRICES,
};
pub use toxicity_agg::ToxicityAggregator;
#[cfg(feature = "polymarket")]
pub use user_data::{
    open_polymarket_user_data_ws, spawn_polymarket_user_data, spawn_polymarket_user_data_tracked,
    spawn_polymarket_user_data_with_resync, spawn_polymarket_user_data_with_resync_tracked,
    PolymarketUserDataResync, WS_USER,
};
#[cfg(feature = "polymarket")]
pub use user_ws::{decode_user, decode_user_with_tracker, user_subscribe_message};
pub use wallet_class::{classify, WalletClass, WalletClassMap};

#[cfg(feature = "polymarket")]
pub use order::{
    build_order, derive_order_id, order_id_hash, order_struct_hash, order_to_json, sign_order,
    sign_order_1271, Order, Side, SignatureType, ZERO_ADDRESS,
};

#[cfg(feature = "polymarket")]
pub use l1::{clob_auth_signature, derive_api_key, ensure_l2, l1_headers, DerivedL2};
#[cfg(feature = "polymarket")]
pub use redeem::{
    redeem_neg_risk_calldata, redeem_positions_calldata, Era, CTF_ADDRESS, CTF_COLLATERAL_ADAPTER,
    NEG_RISK_ADAPTER, NEG_RISK_CTF_COLLATERAL_ADAPTER, PUSD_COLLATERAL, USDC_E_ADDRESS,
};
// The whole ledger vocabulary is re-exported, not just the handle: `RedeemLedger`'s public methods
// name `RedeemState`/`PendingRedeem`/`SettledRedeem`, so they must be publicly reachable too.
#[cfg(feature = "polymarket")]
pub use redeem_confirm::{ChainRedeemConfirmer, RedeemConfirmation, MIN_CONFIRMATIONS};
#[cfg(feature = "polymarket")]
pub use redeem_ledger::{PendingRedeem, RedeemLedger, RedeemState, SettledRedeem};
#[cfg(feature = "polymarket")]
pub use redeem_relayer::{
    build_redeem_request, submit_redeem, RedeemKind, RedeemRequest, RedeemResult,
    DEPOSIT_WALLET_FACTORY, RELAYER_BASE,
};
#[cfg(feature = "polymarket")]
pub use split_merge::{
    build_convert_request, build_split_merge_request, convert_positions_calldata,
    index_set_from_indices, merge_positions_calldata, merge_positions_neg_risk_calldata,
    split_position_calldata, split_position_neg_risk_calldata, submit_convert, submit_split_merge,
    SplitMergeKind,
};
#[cfg(feature = "polymarket")]
pub use vike_bridge_core::eip712::{
    digest, domain_separator, eth_address_from_private_key, hash_struct, keccak256, sign_digest,
    sign_digest_hex,
};
// The venue-enforced order hold, resolved per market into `SymbolProperties::taker_hold_ms` —
// `itode` (250 ms, crypto up/down) and `seconds_delay` (3 s, sports game markets) are TWO
// independent mechanisms; see the module doc before touching either.
pub use taker_hold::{
    fetch_condition_id_for_token, fetch_taker_hold_ms, fetch_token_taker_hold_ms,
    parse_condition_id, parse_itode, parse_seconds_delay, resolve_taker_hold_ms, HOLD_ITODE_MS,
    HOLD_SPORTS_GAME_MS,
};
// The dynamic tick-size regime (CLOB hygiene). WIRED: `live_mount_from_vars` builds one, prices the
// exec thread's submits on it and re-fetches the grid on an off-grid reject; it is returned on
// `PolymarketMount::tick_regime` so a quoting path shares the same cache. Every other entry point
// passes `None`, which is byte-identical to before it was wired.
pub use tick_regime::{is_tick_size_reject, TickRegime};

#[cfg(feature = "polymarket")]
pub use auth::{l2_auth_headers, l2_signature, PolyAuthError};
pub use config::{
    load_polymarket_creds_for_account, load_polymarket_creds_from, poly_env_var_names,
    PolymarketCreds, CLOB_BASE, DATA_API_BASE, GAMMA_BASE, RTDS_WS, WS_MARKET,
};
pub use data::{fetch_book, fetch_midpoint, parse_book, PolyBook};
pub use ws::{apply_update, decode_market, subscribe_message, LevelChange, MarketUpdate};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::POLYMARKET;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("polymarket");
        assert_eq!(super::CAPS, caps);
        // `PolymarketExecutionClient` wires submit/cancel only — no modify/batch/reduce-only. Its
        // live tick feed serves quotes + trades + book (NOT bars, NOT the DOM depth-snapshot lane).
        assert!(!caps.supports_modify);
        assert!(!caps.supports_reduce_only);
        assert!(caps.live_data.quotes && caps.live_data.trades);
        assert!(caps.live_data.book && !caps.live_data.depth);
        assert!(!caps.live_data.bars);
        // Expanded axes (w2-task-5): the CLOB has ONE submit path — `order_type` is never read and
        // every order is a signed limit at `price`. `"market"` is encoded AS-ACCEPTED (it emulates:
        // a sell at 0.0 is marketable — the shape Flatten/MarketExit rely on); trigger kinds would
        // rest as nonsense limits → refused. `accepted_tifs` is all five (Ioc→FOK, Day→GTC are the
        // live coercions; Gtd is emitted as a stub — hence OUT of the honored set). Fully
        // collateralized → margin_modes == [Cash]; no native batch.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit"]);
        assert_eq!(caps.trigger_types, &[] as &[TriggerType]);
        assert_eq!(caps.accepted_tifs.len(), 5);
        assert_eq!(caps.supported_tifs, &[TimeInForce::Gtc, TimeInForce::Fok]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cash]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
