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
//! tree always compiles, and has opened its SDK through a runtime-loaded shim since #1720, where it
//! used to stub internally via `#[cfg(not(fcsdk))]`), the exec tree names
//! `vike_bridge_core::eip712` DIRECTLY at every call site (the ONE shared EIP-712 primitive,
//! itself behind bridge-core's `eip712` feature which the `polymarket` feature enables) — a
//! module that does not exist without the feature, and whose former byte-identical local copy in
//! this crate is deleted, so do not re-create one. The exec tree therefore has no way to compile
//! a stub without it, while the feed tree deliberately names no signing type at all and compiles
//! under `--features feeds` alone.
//!
//! ## Live-vs-parked map (verified against every consumer, 2026-07-28)
//!
//! **LIVE — on a production path today** (composed by `mount`/`recon_client` and registered
//! through vike-tradehub's registry, or consumed directly by vike-run / vike-tradehub /
//! vike-backfill — and by vike-app, the GUI shell, until its venue feeds went on 2026-09-09; the
//! map is otherwise as verified):
//! exec half `client` / `exec` / `user_data` / `user_ws` / `order` / `l1` / `auth` / `config` /
//! `registry` / `ratelimit` / `rate_budget` / `fill_tracker` / `history` / `neg_risk_lookup` /
//! `mount` / `recon_client`; data half `market_feed` / `ws` / `data` / `rtds` / `raw_tap` /
//! `instruments` / `taker_hold` / `filters_rec` / `gamma` / `catalog` / `neg_risk_set` /
//! `universe` / `rewards` / `toxicity_agg` / `wallet_class`.
//!
//! `rate_budget` is LIVE as of the rate-budget wiring: `exec` mirrors both token buckets on
//! every submit/cancel and reconciles them to the venue's `Poly-RateLimit-*` headers. The RESERVE
//! INVARIANT is now DRIVEN too: `gate_cancel_shared` runs it from `client`'s
//! `ExecCommand::Cancel` arm — the door every core cancel comes through — for a
//! `vike_exec::CancelIntent::Routine` cancel and for nothing else, which became possible once the
//! shared `ExecutionClient` seam carried an intent (`cancel_with_intent`). The BULK
//! targeted-vs-`cancel-all` choice is DRIVEN too, and needed a second shared-seam change to get
//! there: `client`'s `ExecCommand::CancelBatch` arm hands a whole batch to `cancel_orders`,
//! which is reachable at all only because `ExecActor` stopped shredding a batch into `n` singles
//! before any venue code ran. See that function's doc for the two declared residuals.
//!
//! **PARKED — built + fixture-tested, NO production caller today** (deliberately held, each
//! waiting on its enablement decision — not dead code): the whole `settlement` cluster (opt-in
//! post-trade money/book movement; its module doc is the cluster contract), `discovery`
//! (rolling-window market discovery), `convert_arb` (neg-risk convert-arb detector), `scoring`
//! (order-scoring reads), `egress`'s expected-egress PROBE half (the module's proxy/agent/
//! `get_json` plumbing is LIVE — re-homed there from `exec` for the feeds/exec seam; every REST
//! call and WS dial rides it, and its `check_order_placement_geo` half is LIVE too, since the exec
//! mount refuses on a `blocked` answer), and `heartbeat` (server-side dead-man
//! switch). TWO modules LEFT this list: `tick_regime` — the live mount now builds one and the exec
//! thread prices submits on it — and `rate_budget`, whose client-side mirror the exec actor now
//! drives end-to-end (submit gate, cancel-token reserve, targeted-over-`cancel-all`, header
//! reconcile), not just `rate_budget::parse_headers`. See each module's doc and `mount`.

// The feeds/exec seam (split-plane Phase-5 hardening): the crate-level gate is now the `feeds`
// feature — the keyless market-data plane, which everything else in this crate builds on — and
// every module that signs (EIP-712 / L2-HMAC), pumps private user-data, mounts, reconciles or
// settles sits behind the additional `polymarket` feature (which implies `feeds`), so a
// `--features feeds` consumer links the feed surface only and vike-bridge-core's `eip712` (the
// whole k256/keccak stack) never enters its binary. A no-feature build stays an EMPTY crate,
// exactly as before the split. The `bridges-feeds` arm of scripts/ci_feature_suite.sh is the
// gate, including the structural no-signer-crate cargo-tree check.
#![cfg(feature = "feeds")]

// The EXEC plane — order signing, submission, fills, reconcile and settlement — compiled only
// under `polymarket`; everything else in this crate is the keyless `feeds` plane (owner decision
// D5 named the directory).
pub mod catalog;
mod config;
mod data;
pub mod discovery;
mod egress;
#[cfg(feature = "polymarket")]
pub mod exec_plane;
pub mod filters_rec;
pub mod gamma;
mod instruments;
mod market_feed;
pub mod neg_risk_lookup;
pub mod neg_risk_set;
pub mod raw_tap;
pub mod rewards;
pub mod rolling_family;
pub mod rtds;
pub mod taker_hold;
pub mod tick_regime;
pub mod toxicity_agg;
pub mod universe;
pub mod wallet_class;
mod ws;

pub use catalog::{MarketCatalog, PolymarketCatalog, markets_to_instruments};
pub use discovery::{
    FetchSpec, GammaSlugResolver, GammaSource, MarketFilter, PredicateFilter, RollingTick,
    RollingWindowPlanner, SearchFilter, SlugPrefixFilter, TagFilter, WindowResolver, WindowSpec,
    render_slug,
};
pub use egress::{
    DEFAULT_EGRESS_PROBE, DUBLIN_EGRESS_COUNTRY, Egress, EgressCheck, EgressSettings, GEOBLOCK_URL,
    Geoblock, GeoblockVerdict, check_expected_egress, check_order_placement_geo, declare_egress,
    declare_from_rows, egress_legacy_names, geoblock_verdict, get_json, observe_egress,
    observe_geoblock, parse_egress, parse_geoblock, proxy_url, ws_proxy,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::client::{
    DEFAULT_HISTORY_LIMIT as EXEC_DEFAULT_HISTORY_LIMIT, PolymarketExecutionClient,
    PolymarketLiveConfig, UserChannelConfig,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::convert_arb::{
    ConvertArbConfig, ConvertArbOpportunity, LockKind, SizePolicy, SizedLock,
    detect as detect_convert_arb, evaluate_lock, sum_best_asks,
};
#[cfg(feature = "polymarket")]
// NOTE no `CancelIntent` here any more: the classification is `vike_exec::CancelIntent`, on the
// shared `ExecutionClient` seam, and this crate's byte-identical local copy was DELETED rather than
// re-exported (the workspace's no-`pub use`-shim-on-a-move rule — a second name for one fact rots).
pub use exec_plane::exec::{
    CancelBatch, CancelPlan, CancelScope, POLY_RATE_GATE_ENV, cancel_all_orders, cancel_body,
    cancel_order, cancel_order_relayer, cancel_orders, gate_cancel, gate_cancel_shared, get_orders,
    get_signed, get_trades, plan_cancels, rate_gate_enforced_in, rate_gate_would_block_count,
    submit_body, submit_order, submit_order_relayer,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::fill_tracker::{
    DEFAULT_DUST_SNAP_THRESHOLD, DUST_FRACTION_OF_SUBMITTED, DUST_TRADE_ID_SUFFIX, FillTracker,
    SnapOutcome,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::auto_redeem::{
    AutoRedeemPoller, ProdDeps, RedeemDeps, RedeemTickReport, kill_switch_tripped, redeem_once,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::chain::{
    ChainOracle, ChainResolution, ChainRpcSettings, ChainSettlement, ChainTick, ChainWatchPoller,
    ChainWatcher, DEFAULT_RPC_URL, PolygonRpc, RedeemVenue, Redemption, ResolutionEvent,
    TOPIC_CONDITION_RESOLUTION, TOPIC_CTF_PAYOUT_REDEMPTION, TOPIC_NEG_RISK_PAYOUT_REDEMPTION,
    TokenTransfer, decode_condition_resolution, decode_ctf_redemption, decode_neg_risk_redemption,
    decode_redemption, decode_token_transfer, join_settlement,
};
pub use filters_rec::{record_token_properties, record_token_tick};
pub use gamma::{
    GammaClient, GammaMarket, decode_json_string_array, decode_json_string_f64_array,
    neg_risk_question_id, parse_gamma_events, parse_gamma_markets,
};
// The OPT-IN server-side dead-man heartbeat: nothing spawns a beat unless its caller passes
// `enabled` AND creds are present — and no composition root does — so a default build never posts
// `/v1/heartbeats` and is byte-identical.
#[cfg(feature = "polymarket")]
pub use exec_plane::heartbeat::{
    BeatReport, DEFAULT_BEAT_INTERVAL, HEARTBEAT_PATH, HeartbeatPoller, HeartbeatState,
    HeartbeatTransport, ProdTransport, beat_once,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::history::map_polymarket_history;
#[cfg(feature = "polymarket")]
pub use exec_plane::mount::{
    GeoblockAction, POLY_EXEC_ENV, POLY_EXEC_MARKETS_ENV, POLY_GEOBLOCK_OVERRIDE_ENV,
    POLY_PRESUBMIT_REGISTER_ENV, PolymarketMount, geoblock_action, geoblock_override_enabled,
    live_mount_for_account, live_mount_from_vars, poly_exec_enabled, poly_exec_markets,
    presubmit_register_enabled,
};
pub use instruments::{
    PolyMarket, PolyToken, fetch_all_markets, fetch_markets_page, fetch_tick_size_direct,
    fetch_token_neg_risk, fetch_token_tick_size, fetch_token_tick_size_paged, is_neg_risk,
    next_cursor, parse_markets, parse_neg_risk, parse_tick_size,
};
pub use market_feed::{
    DEFAULT_TOKENS_PER_SOCKET, Feeds, MarketStream, PumpMode, PumpTiming, TokenSlot, TokenState,
    Watchdog, run_session, run_shard_session, tokens_per_socket,
};
pub use neg_risk_lookup::{NegRiskFetch, NegRiskSource, clob_neg_risk_fetch};
pub use neg_risk_set::{NegRiskMember, NegRiskSet, SetCompleteness};
pub use universe::{
    RankBy, SelectedMarket, UniverseDiff, UniverseManager, UniversePolicy, select_universe,
};
// crate-root re-export so raw_tap.rs (and any future sibling module) can reach the venue tag via
// `crate::VENUE` without duplicating the literal.
#[cfg(feature = "polymarket")]
pub use exec_plane::rate_budget::{
    CancelDecision, CancelStrategy, RateBudget, RateLimitSignal, SubmitDecision, Tier,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::recon_client::{
    POLY_RECONCILE_ENV, PolymarketReconClient, poly_reconcile_enabled, recon_client,
    recon_client_for_account, recon_client_from_vars, settlement_fill_report,
    signature_type_for_account, signature_type_from_vars,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::registry::PolymarketRegistry;
#[cfg(feature = "polymarket")]
pub use exec_plane::scoring::{
    ORDER_SCORING_PATH, ORDERS_SCORING_PATH, fetch_order_scoring, fetch_orders_scoring,
    order_scoring_query, orders_scoring_query, parse_order_scoring, parse_orders_scoring,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::positions::{
    Payout, Position, PositionsClient, WinnerSource, parse_positions, payout_of, redeemable,
    redeemable_by, resolved_candidates,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::resolve::{
    ChainResolveDeps, DEFAULT_POLL_INTERVAL, LOSER_PAYOUT, PayoutSource, ProdResolveDeps,
    ResolveDeps, ResolvePoller, ResolveWatchlist, SettleTickReport, SettlementLedger,
    WINNER_PAYOUT, WatchEntry, ambiguous_conditions, payout_for, resolved_conditions, settle_once,
    settlement_fill, settlement_key, settlement_trade_id, winning_tokens,
};
pub(crate) use market_feed::VENUE;
pub use raw_tap::{RawCaptureConfig, RawTap, RawTapHandle, RawTapOwner, TappedStream};
pub use rewards::RewardsConfig;
// The OPT-IN RTDS underlying reference-price feed: nothing constructs an `RtdsFeed`, so a build
// that never calls `RtdsFeed::start` never dials it and never emits a single sink call.
#[cfg(feature = "polymarket")]
pub use exec_plane::user_data::{
    WS_USER, open_polymarket_user_data_ws, spawn_polymarket_user_data,
    spawn_polymarket_user_data_tracked, spawn_polymarket_user_data_with_resync,
    spawn_polymarket_user_data_with_resync_tracked,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::user_ws::{decode_user, decode_user_with_tracker, user_subscribe_message};
pub use rtds::{
    ActivityTrade, ActivityTradeSink, RTDS_CRYPTO_SYMBOLS_OBSERVED, RTDS_IDLE_THRESHOLD,
    RTDS_KEEPALIVE_INTERVAL, RTDS_PING, RTDS_TYPE_SUBSCRIBE, RTDS_TYPE_TRADES, RTDS_TYPE_UPDATE,
    RefPrice, RtdsActivityFeed, RtdsConfig, RtdsFeed, TOPIC_ACTIVITY, TOPIC_CRYPTO_PRICES,
    TOPIC_CRYPTO_PRICES_CHAINLINK, TOPIC_EQUITY_PRICES, TradeSide, decode_activity_trades,
    decode_ref_prices, normalize_ts_ms, rtds_subscribe_message, rtds_subscribe_message_filtered,
    rtds_symbol_filter, run_rtds_activity_session, run_rtds_session,
};
pub use toxicity_agg::ToxicityAggregator;
pub use wallet_class::{WalletClass, WalletClassMap, classify};

#[cfg(feature = "polymarket")]
pub use exec_plane::order::{
    Order, Side, SignatureType, ZERO_ADDRESS, build_order, derive_order_id, order_id_hash,
    order_struct_hash, order_to_json, sign_order, sign_order_1271,
};

#[cfg(feature = "polymarket")]
pub use exec_plane::l1::{DerivedL2, clob_auth_signature, derive_api_key, ensure_l2, l1_headers};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::redeem::{
    CTF_ADDRESS, CTF_COLLATERAL_ADAPTER, Era, NEG_RISK_ADAPTER, NEG_RISK_CTF_COLLATERAL_ADAPTER,
    PUSD_COLLATERAL, USDC_E_ADDRESS, redeem_neg_risk_calldata, redeem_positions_calldata,
};
// The whole ledger vocabulary is re-exported, not just the handle: `RedeemLedger`'s public methods
// name `RedeemState`/`PendingRedeem`/`SettledRedeem`, so they must be publicly reachable too.
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::redeem_confirm::{
    ChainRedeemConfirmer, MIN_CONFIRMATIONS, RedeemConfirmation,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::redeem_ledger::{
    PendingRedeem, RedeemLedger, RedeemState, SettledRedeem,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::redeem_relayer::{
    DEPOSIT_WALLET_FACTORY, RELAYER_BASE, RedeemKind, RedeemRequest, RedeemResult,
    build_redeem_request, submit_redeem,
};
#[cfg(feature = "polymarket")]
pub use exec_plane::settlement::split_merge::{
    SplitMergeKind, build_convert_request, build_split_merge_request, convert_positions_calldata,
    index_set_from_indices, merge_positions_calldata, merge_positions_neg_risk_calldata,
    split_position_calldata, split_position_neg_risk_calldata, submit_convert, submit_split_merge,
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
    HOLD_ITODE_MS, HOLD_SPORTS_GAME_MS, fetch_condition_id_for_token, fetch_taker_hold_ms,
    fetch_token_taker_hold_ms, parse_condition_id, parse_itode, parse_seconds_delay,
    resolve_taker_hold_ms,
};
// The dynamic tick-size regime (CLOB hygiene). WIRED: `live_mount_from_vars` builds one, prices the
// exec thread's submits on it and re-fetches the grid on an off-grid reject; it is returned on
// `PolymarketMount::tick_regime` so a quoting path shares the same cache. Every other entry point
// passes `None`, which is byte-identical to before it was wired.
pub use tick_regime::{TickRegime, is_tick_size_reject};

pub use config::{
    CLOB_BASE, DATA_API_BASE, GAMMA_BASE, PolymarketCreds, RTDS_WS, WS_MARKET,
    load_polymarket_creds_for_account, load_polymarket_creds_from, poly_env_var_names,
};
pub use data::{PolyBook, fetch_book, fetch_midpoint, parse_book};
#[cfg(feature = "polymarket")]
pub use exec_plane::auth::{PolyAuthError, l2_auth_headers, l2_signature};
pub use ws::{LevelChange, MarketUpdate, apply_update, decode_market, subscribe_message};

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
