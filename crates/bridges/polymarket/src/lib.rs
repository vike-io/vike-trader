//! Polymarket CLOB venue adapter (prediction markets). Native, pure-Rust — reference spec is the
//! official `polymarket-client-sdk-v2` (NOT a runtime dep). Behind TWO crate-local cargo features
//! since the split-plane Phase-5 seam: `feeds` (the keyless market-data plane) and `polymarket`
//! (the full venue, implying `feeds`).
//!
//! DATA half: unauthenticated CLOB book/midpoint reads (the shared `RestTransport` seam) and the
//! market-channel WS book feed (`ws`). EXEC half: EIP-712 order signing (`order`/`l1`, over the
//! shared `vike_bridge_core::eip712` — CLOB **V2** domain/struct), L2-HMAC submit/cancel
//! (`auth`/`exec`), and the authenticated user-channel WS pump (`user_data`/`user_websocket`). Symbols
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
//! **LIVE — on a production path today** (composed by `adapter`/`recon_client` and registered
//! through vike-tradehub's registry, or consumed directly by vike-tradehub / vike-backfill — and
//! by vike-run until its polymarket bins and their bridge edge were deleted on 2026-09-28, and by
//! vike-app, the GUI shell, until its venue feeds went on 2026-09-09; the map is otherwise as
//! verified):
//! exec half `client` / `exec` / `user_data` / `user_websocket` / `order` / `l1` / `auth` / `config` /
//! `registry` / `ratelimit` / `rate_budget` / `fill_tracker` / `history` / `neg_risk_lookup` /
//! `adapter` / `recon_client`; data half `market_feed` / `ws` / `data` / `rtds` / `raw_tap` /
//! `instruments` / `taker_hold` / `filters_rec` / `gamma` / `catalog` / `neg_risk_set` /
//! `universe` / `rewards`. The flow-toxicity aggregator and its wallet classes live in `vike-market-making`
//! (`flow_toxicity`, `wallet_class`) since B3.22.
//!
//! `rate_budget` is LIVE as of the rate-budget wiring: `exec` mirrors both token buckets on
//! every submit/cancel and reconciles them to the venue's `Poly-RateLimit-*` headers. The RESERVE
//! INVARIANT is now DRIVEN too: `gate_cancel_shared` runs it from `client`'s
//! `ExecutionClientActorCommand::Cancel` arm — the door every core cancel comes through — for a
//! `vike_model::OrderCancelIntent::Routine` cancel and for nothing else, which became possible once the
//! shared `ExecutionClient` seam carried an intent (`cancel_with_intent`). The BULK
//! targeted-vs-`cancel-all` choice is DRIVEN too, and needed a second shared-seam change to get
//! there: `client`'s `ExecutionClientActorCommand::CancelBatch` arm hands a whole batch to `cancel_orders`,
//! which is reachable at all only because `ExecutionClientActor` stopped shredding a batch into `n` singles
//! before any venue code ran. See that function's doc for the two declared residuals.
//!
//! **PARKED — built + fixture-tested, NO production caller today** (deliberately held, each
//! waiting on its enablement decision — not dead code): the whole `settlement` cluster (opt-in
//! post-trade money/book movement; its module doc is the cluster contract), `discovery`
//! (rolling-window market discovery), `convert_arb` (the wiring of `vike_arbitrage`'s outcome-set lock to a neg-risk set), `scoring`
//! (order-scoring reads), `egress`'s expected-egress PROBE half (the module's proxy/agent/
//! `get_json` plumbing is LIVE — re-homed there from `exec` for the feeds/exec seam; every REST
//! call and WS dial rides it, and its `check_order_placement_geo` half is LIVE too, since the exec
//! start refuses on a `blocked` answer), and `heartbeat` (server-side dead-man
//! switch). TWO modules LEFT this list: `tick_regime` — the live start now builds one and the exec
//! thread prices submits on it — and `rate_budget`, whose client-side mirror the exec actor now
//! drives end-to-end (submit gate, cancel-token reserve, targeted-over-`cancel-all`, header
//! reconcile), not just `rate_budget::parse_headers`. See each module's doc and `adapter`.

// The feeds/exec seam (split-plane Phase-5 hardening): the crate-level gate is now the `feeds`
// feature — the keyless market-data plane, which everything else in this crate builds on — and
// every module that signs (EIP-712 / L2-HMAC), pumps private user-data, starts, reconciles or
// settles sits behind the additional `polymarket` feature (which implies `feeds`), so a
// `--features feeds` consumer links the feed surface only and vike-bridge-core's `eip712` (the
// whole k256/keccak stack) never enters its binary. A no-feature build stays an EMPTY crate,
// exactly as before the split. The `bridges-feeds` arm of scripts/ci_feature_suite.sh is the
// gate, including the structural no-signer-crate cargo-tree check.
#![cfg(feature = "feeds")]
#![warn(unreachable_pub)]

// The EXEC plane — order signing, submission, fills, reconcile and settlement — compiled only
// under `polymarket`; everything else in this crate is the keyless `feeds` plane (owner decision
// D5 named the directory).
pub mod catalog;
mod config;
pub mod neg_risk_lookup;
// Every refusal this crate makes, its id and its one text (`vike_utility::refusal_enum!`).
pub mod refusals;
// What the gatewayhub asks of polymarket to admit any outcome token (keyless point reads): the venue's list spec.
pub mod symbol_properties;
// The ingest-lane wait the live smokes share, compiled once here instead of once per smoke; only
// the self dev-dependency turns `test-support` on, so no shipped build compiles it.
#[cfg(feature = "test-support")]
pub mod test_support;

#[cfg(feature = "polymarket")]
pub(crate) mod account;
#[cfg(feature = "polymarket")]
pub mod adapter;
#[cfg(feature = "polymarket")]
pub mod execution;
pub mod feed;
pub mod instrument;
#[cfg(feature = "polymarket")]
pub mod reconcile;
// The 9-module opt-in settlement cluster — chain / resolve / auto_redeem / redeem / redeem_confirm
// / redeem_ledger / redeem_relayer / split_merge / positions — lives under settlement/; its module
// doc states the cluster contract ONCE (opt-in env gates, default OFF, no composition-root wiring
// today). Its modules are named by ONE path, `settlement::<module>` (the owner's 2026-09-18
// no-alias ruling): the module aliases that kept the pre-move root paths resolving were deleted
// 2026-09-27. The flat ITEM re-exports further down are crate-root vocabulary and stay.
#[cfg(feature = "polymarket")]
pub mod settlement;
pub mod transport;

pub use catalog::discovery::{
    FetchSpec, GammaSource, MarketFilter, MarketFilterPredicate, MarketFilterSearch,
    MarketFilterSlugPrefix, MarketFilterTag, RollingTick, RollingWindowPlanner, WindowResolver,
    WindowResolverGammaSlug, WindowSpec, render_slug,
};
pub use catalog::gamma::{
    GammaClient, GammaMarket, decode_json_string_array, decode_json_string_f64_array,
    neg_risk_question_id, parse_gamma_events, parse_gamma_markets,
};
pub use catalog::{CatalogProviderPolymarket, MarketCatalog, markets_to_instruments};
#[cfg(feature = "polymarket")]
pub use execution::fill_tracker::{
    DEFAULT_DUST_SNAP_THRESHOLD, DUST_FRACTION_OF_SUBMITTED, DUST_TRADE_ID_SUFFIX, FillTracker,
    SnapOutcome,
};
#[cfg(feature = "polymarket")]
// NOTE no `OrderCancelIntent` here any more: the classification is `vike_model::OrderCancelIntent`, on the
// shared `ExecutionClient` seam, and this crate's byte-identical local copy was DELETED rather than
// re-exported (the workspace's no-`pub use`-shim-on-a-move rule — a second name for one fact rots).
pub use execution::rest::{
    CancelBatch, CancelFailure, CancelPlan, CancelScope, cancel_body, cancel_order,
    cancel_order_relayer, cancel_orders, gate_cancel, gate_cancel_shared, get_orders, get_signed,
    get_trades, plan_cancels, rate_gate_enforced, rate_gate_would_block_count, submit_body,
    submit_order, submit_order_relayer,
};
#[cfg(feature = "polymarket")]
pub use execution::{PolymarketLiveConfig, UserChannelConfig};
pub use instrument::record::{record_token_properties, record_token_tick};
#[cfg(feature = "polymarket")]
pub use settlement::auto_redeem::{
    AutoRedeemPoller, RedeemDependencies, RedeemDependenciesProduction, RedeemTickReport,
    kill_switch_tripped, redeem_once,
};
#[cfg(feature = "polymarket")]
pub use settlement::chain::{
    ChainOracle, ChainResolution, ChainRpcSettings, ChainSettlement, ChainTick, ChainWatchPoller,
    ChainWatcher, PolygonRpc, RedeemVenue, Redemption, ResolutionEvent, TOPIC_CONDITION_RESOLUTION,
    TOPIC_CTF_PAYOUT_REDEMPTION, TOPIC_NEG_RISK_PAYOUT_REDEMPTION, TokenTransfer,
    decode_condition_resolution, decode_ctf_redemption, decode_neg_risk_redemption,
    decode_redemption, decode_token_transfer, join_settlement,
};
pub use transport::{
    DEFAULT_EGRESS_PROBE, DUBLIN_EGRESS_COUNTRY, Egress, EgressCheck, EgressSettings, Geoblock,
    GeoblockVerdict, check_expected_egress, check_order_placement_geo, declare_egress,
    declare_from_rows, geoblock_verdict, get_json, observe_egress, observe_geoblock, parse_egress,
    parse_geoblock, proxy_url, websocket_proxy,
};
// The OPT-IN server-side dead-man heartbeat: nothing spawns a beat unless its caller passes
// `enabled` AND creds are present — and no composition root does — so a default build never posts
// `/v1/heartbeats` and is byte-identical.
#[cfg(feature = "polymarket")]
pub use adapter::{
    GeoblockAction, POLYMARKET_GEOBLOCK_OVERRIDE_ENV, PolymarketLiveRuntime, geoblock_action,
    geoblock_override_enabled, live_runtime_for_account, polymarket_execution_enabled,
    polymarket_execution_markets, presubmit_register_enabled,
};
pub use catalog::neg_risk_set::{NegRiskMember, NegRiskSet, SetCompleteness};
pub use catalog::universe::{
    RankBy, SelectedMarket, UniverseDiff, UniverseManager, UniversePolicy, select_universe,
};
#[cfg(feature = "polymarket")]
pub use execution::heartbeat::{
    BeatReport, DEFAULT_BEAT_INTERVAL, HEARTBEAT_PATH, HeartbeatPoller, HeartbeatState,
    HeartbeatTransport, HeartbeatTransportProduction, beat_once,
};
#[cfg(feature = "polymarket")]
pub use execution::resync::map_polymarket_history;
pub use feed::session::{FeedTiming, Watchdog, run_session};
pub use feed::state::{FeedLane, TokenSlot, TokenState};
pub use feed::{
    DEFAULT_TOKENS_PER_SOCKET, DataClientBusSpecHoldPolymarket, DataClientBusSpecPolymarket,
    run_shard_session, run_token_shard, tokens_per_socket,
};
pub use instrument::properties::{
    PolymarketMarket, PolymarketToken, fetch_all_markets, fetch_markets_page,
    fetch_tick_size_direct, fetch_token_neg_risk, fetch_token_tick_size,
    fetch_token_tick_size_paged, next_cursor, parse_markets, parse_neg_risk, parse_tick_size,
};
pub use neg_risk_lookup::{NegRiskFetch, NegRiskSource, clob_neg_risk_fetch};
// crate-root re-export so feed/tap.rs (and any future sibling module) can reach the venue tag via
// `crate::VENUE` without duplicating the literal.
#[cfg(feature = "polymarket")]
pub use execution::registry::OrderVenueIdTracker;
#[cfg(feature = "polymarket")]
pub use execution::scoring::{
    ORDER_SCORING_PATH, ORDERS_SCORING_PATH, fetch_order_scoring, fetch_orders_scoring,
    order_scoring_query, orders_scoring_query, parse_order_scoring, parse_orders_scoring,
};
pub(crate) use feed::VENUE;
pub use feed::tap::{
    RawCaptureConfig, RawTap, RawTapHandle, RawTapOwner, WebsocketSessionMarketStreamTapped,
};
pub use instrument::rewards::RewardsConfig;
#[cfg(feature = "polymarket")]
pub use reconcile::{
    ReconcileClientPolymarket, polymarket_reconcile_enabled, recon_client,
    reconcile_client_for_account, settlement_fill_report, signature_type_for_account,
};
#[cfg(feature = "polymarket")]
pub use settlement::positions::{
    Payout, Position, PositionsClient, WinnerSource, parse_positions, payout_of, redeemable,
    redeemable_by, resolved_candidates,
};
#[cfg(feature = "polymarket")]
pub use settlement::resolve::{
    DEFAULT_POLL_INTERVAL, LOSER_PAYOUT, PayoutSource, ResolveDependencies,
    ResolveDependenciesChain, ResolveDependenciesProduction, ResolvePoller, ResolveWatchlist,
    SETTLEMENT_LEDGER_LOG, SettleTickReport, WINNER_PAYOUT, WatchEntry, ambiguous_conditions,
    payout_for, resolved_conditions, settle_once, settlement_fill, settlement_key,
    settlement_trade_id, winning_tokens,
};
#[cfg(feature = "polymarket")]
pub use transport::ratelimit::budget::{
    CancelDecision, CancelStrategy, RateBudget, RateLimitSignal, SubmitDecision, Tier,
};
// The OPT-IN RTDS underlying reference-price feed: nothing constructs an `FeedRtds`, so a build
// that never calls `FeedRtds::start` never dials it and never emits a single sink call.
#[cfg(feature = "polymarket")]
pub use execution::user_data::decode::{
    decode_user, decode_user_with_tracker, user_subscribe_message,
};
#[cfg(feature = "polymarket")]
pub use execution::user_data::{
    USER_CHANNEL_PING, open_polymarket_user_data_websocket,
    spawn_polymarket_user_data_with_resync_tracked,
};
pub use feed::rtds::{
    ActivityTrade, ActivityTradeSink, FeedRtds, FeedRtdsActivity, RTDS_IDLE_THRESHOLD,
    RTDS_KEEPALIVE_INTERVAL, RTDS_PING, RTDS_TYPE_SUBSCRIBE, RTDS_TYPE_TRADES, RTDS_TYPE_UPDATE,
    RefPrice, RtdsConfig, TOPIC_ACTIVITY, TOPIC_CRYPTO_PRICES, TOPIC_CRYPTO_PRICES_CHAINLINK,
    TOPIC_EQUITY_PRICES, decode_activity_trades, decode_ref_prices, normalize_ts_ms,
    rtds_subscribe_message, rtds_subscribe_message_filtered, rtds_symbol_filter,
    run_rtds_activity_session, run_rtds_session,
};

#[cfg(feature = "polymarket")]
pub use execution::order::{
    OrderSideWire, PolymarketOrder, SignatureType, build_order, derive_order_id, order_id_hash,
    order_struct_hash, order_to_json, sign_order, sign_order_1271,
};

#[cfg(feature = "polymarket")]
pub use settlement::redeem::{
    CTF_ADDRESS, CTF_COLLATERAL_ADAPTER, Era, NEG_RISK_ADAPTER, NEG_RISK_CTF_COLLATERAL_ADAPTER,
    PUSD_COLLATERAL, USDC_E_ADDRESS, redeem_neg_risk_calldata, redeem_positions_calldata,
};
#[cfg(feature = "polymarket")]
pub use transport::sign::l1::{
    DerivedL2, clob_auth_signature, derive_api_key, ensure_l2, l1_headers,
};
// The whole ledger vocabulary is re-exported, not just the handle: `RedeemLedger`'s public methods
// name `RedeemState`/`PendingRedeem`/`SettledRedeem`, so they must be publicly reachable too.
#[cfg(feature = "polymarket")]
pub use settlement::redeem_confirm::{ChainRedeemConfirmer, MIN_CONFIRMATIONS, RedeemConfirmation};
#[cfg(feature = "polymarket")]
pub use settlement::redeem_ledger::{PendingRedeem, RedeemLedger, RedeemState, SettledRedeem};
#[cfg(feature = "polymarket")]
pub use settlement::redeem_relayer::{
    DEPOSIT_WALLET_FACTORY, RedeemKind, RedeemRequest, RedeemResult, build_redeem_request,
    submit_redeem,
};
#[cfg(feature = "polymarket")]
pub use settlement::split_merge::{
    SplitMergeKind, build_convert_request, build_split_merge_request, convert_positions_calldata,
    index_set_from_indices, merge_positions_calldata, merge_positions_neg_risk_calldata,
    split_position_calldata, split_position_neg_risk_calldata, submit_convert, submit_split_merge,
};
// The venue-enforced order hold, resolved per market into `SymbolProperties::taker_hold_ms` —
// `itode` (250 ms, crypto up/down) and `seconds_delay` (3 s, sports game markets) are TWO
// independent mechanisms; see the module doc before touching either.
pub use instrument::taker_hold::{
    fetch_condition_id_for_token, fetch_taker_hold_ms, fetch_token_taker_hold_ms,
    parse_condition_id, parse_itode, parse_seconds_delay, resolve_taker_hold_ms,
};
// The dynamic tick-size regime (CLOB hygiene). WIRED: `live_runtime_for_account` builds one, prices the
// exec thread's submits on it and re-fetches the grid on an off-grid reject; it is returned on
// `PolymarketLiveRuntime::tick_regime` so a quoting path shares the same cache. Every other entry point
// passes `None`, which is byte-identical to before it was wired.
pub use instrument::tick_regime::{TickRegime, is_tick_size_reject};

pub use config::{PolymarketConfig, load_polymarket_credentials_from};
pub use feed::decode::{LevelChange, MarketUpdate, decode_market, subscribe_message};
pub use feed::depth::{PolyBook, fetch_book, fetch_midpoint, parse_book};
#[cfg(feature = "polymarket")]
pub use transport::sign::l2::{PolymarketAuthenticationError, l2_auth_headers, l2_signature};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::POLYMARKET;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("polymarket");
        assert_eq!(super::CAPABILITIES, capabilities);
        // The exec client (`spawn_live`) wires submit/cancel only — no modify/batch/reduce-only. Its
        // live tick feed serves quotes + trades + book (NOT bars, NOT the DOM depth-snapshot lane).
        assert!(!capabilities.supports_modify);
        assert!(!capabilities.supports_reduce_only);
        assert!(capabilities.live_data.quotes && capabilities.live_data.trades);
        assert!(capabilities.live_data.book && !capabilities.live_data.depth);
        assert!(!capabilities.live_data.bars);
        // Expanded axes (w2-task-5): the CLOB has ONE submit path — every order is a signed limit at
        // `price`, and `order_type` only refuses a market order naming one. `"market"` is encoded AS-ACCEPTED (it emulates:
        // a sell at 0.0 is marketable — the shape Flatten/MarketExit rely on); trigger kinds would
        // rest as nonsense limits → refused. `accepted_tifs` is all five (Ioc→FOK, Day→GTC are the
        // live coercions; Gtd is emitted as a stub — hence OUT of the honored set). Fully
        // collateralized → margin_modes == [Cash]; no native batch.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(capabilities.supported_order_kinds, &["market", "limit"]);
        assert_eq!(capabilities.trigger_types, &[] as &[TriggerType]);
        assert_eq!(capabilities.accepted_tifs.len(), 5);
        assert_eq!(capabilities.supported_tifs, &[TimeInForce::Gtc, TimeInForce::Fok]);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cash]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(!capabilities.supports_post_only);
    }
}
