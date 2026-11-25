//! Binance adapter — spot REST (slice 1) + WS-API user data (slice 2) + perp (slice 3).
//!
//! DATA half: [`history`] (REST kline/candlestick history) and [`feed::DataClientSpecBinance`] (the
//! live `vike_data::DataClient`'s venue half, run by the one data-client engine: kline, mark, depth and
//! trades — trades via [`family::feed::tick::trades::run_trades_feed`], Task
//! B2). EXEC half: [`execution::rest::spot::VenueRestBinanceSpot`] (signed spot REST),
//! [`execution::rest::perp::VenueRestBinancePerp`] (signed USDS-M futures REST, incl. native batch-submit+cancel and
//! native modify), [`execution::user_data::spot`]/[`execution::user_data::perp`] (WS-API/listenKey private-stream sessions),
//! and [`execution::resync`] (audit-A3 resync replay). Also owns the R8 HFT public L2/tick track
//! ([`feed::tick`]).
//!
//! Moved into crates/bridges/binance (crate-reorg Phase 3, PR H). Binance was the cleave DONOR: its
//! own venue-neutral fragments (`WsSocket`/`WebsocketSessionUserStreamTungstenite`, `SymbolProperties`/`VenueRest`/
//! `ExecutionClientRest<R>`, `kline_to_bar`) were relocated into `vike-bridge-core` in PR A so every
//! sibling venue (bybit/okx/deribit/polymarket) kept compiling as it moved out on its own — the
//! remaining module here is self-contained venue code, same template as bybit/deribit/okx.

pub mod catalog;
// The shared Binance-wire-grammar core vike-aster reuses — NOT a venue. Everything Aster forked
// verbatim lives there once and both venues' same-named modules are thin re-exports/callers over
// it: the four pure mappers (`event_mapper`/`perp_mapper`/`history`/`filters_rec`, rung 1, venue as
// a param), the public market-data stack (`market_data`/`trades`/`market_feed`, rung 2, hosts as
// a `UrlTable` param — Binance's row is built from its `venue_url` rows, never the environment), and
// the public catalog parse (`catalog`, rung 3 — the exchangeInfo URLs are built here from the rows,
// only the parse is shared). `ratelimit` is deliberately NOT shared: see the family module doc.
pub mod family;
// The venue-adapter contract impl (docs/decisions/0096): it asks the withdraw-permission probe and
// builds the exec and reconcile clients, so it rides `exec` with them.
#[cfg(feature = "execution")]
pub mod adapter;
// What the gatewayhub asks of binance to admit any symbol (keyless lists): the venue's list spec.
pub mod symbol_properties;

// The exec/feeds seam (ruling 8 of the datahub market-data wire design), the same shape
// vike-hyperliquid and vike-aster already carry: everything that signs, pumps private user-data or
// reconciles an account sits behind the default-on `execution` feature, so a `default-features = false`
// consumer links the FEED surface only — data/market_data/market_feed/trades/catalog/ratelimit over
// the keyless public endpoints. The `bridges-feeds` arm of scripts/ci_feature_suite.sh is the gate.
//
// ⚠ Unlike hl/aster this removes no CRATE from the dependency tree — binance's signer is
// vike-bridge-core's HMAC one, and the feeds half needs that crate for its transport (it has no
// `full` feature to split since docs/decisions/0135). What the seam removes is COMPILED CODE. Cargo.toml's [features] block
// carries the measurement and the shared-home change that would close the rest.
#[cfg(feature = "execution")]
pub mod account;
#[cfg(feature = "execution")]
pub mod execution;
pub mod feed;
pub mod history;
pub mod instrument;
#[cfg(feature = "execution")]
pub mod reconcile;
// Every refusal this crate makes, its id and its one text (`vike_utility::refusal_enum!`); both planes raise them.
pub mod refusals;
pub mod transport;

/// This adapter's canonical venue id — the string stamped on every event, tick and store row.
///
/// It lived in [`execution::rest::spot`] until the exec/feeds seam landed, where a FEEDS-only build cannot see it:
/// [`history`] (the keyless kline REST history) builds its `family::history::klines::KlineSpec` from it, and
/// [`execution::rest::spot`] is the signed spot REST client. A venue's identity is a fact about the CRATE rather
/// than about one of its REST clients, so it sits here beside [`CAPABILITIES`], the other declaration of
/// what this adapter IS. No re-export was left behind at the old path (CLAUDE.md's no-shim rule);
/// every call site moved.
///
/// ⚠ **This is the ONE definition, and the first writing of this move left three.** `spot`'s was
/// not deleted — only copied here — so the string the SIGNED order path sends and the string a
/// feeds build stamps on a bar became two `pub` consts that could be edited apart; [`feed::tick`]
/// carried a private third. A venue id that differs between the plane that places an order and the
/// plane that records it mis-attributes every event downstream of it, and nothing in a compile
/// would say so. `crates/bridges/binance/src/execution/rest/spot.rs` and
/// `crates/bridges/binance/src/feed/tick/mod.rs` now both `use crate::VENUE`.
pub const VENUE: &str = "binance";

// The live market-DATA seam: `feed::DataClientSpecBinance` is binance's half of the one
// data-client engine (`vike_bridge_core::data_client::ws::DataClientWebsocket`), which implements
// `vike_data::DataClient` (the per-venue `*MarketData` thin-factory wrapper was retired — Phase 1).

// `trades` is `pub(crate)` (its module-internal types like `AggTrade` stay crate-private), but the
// production backfill entry point (SP3 Task 3) is re-exported here so a caller outside this crate
// can reach it as `vike_binance::agg_trades_backfill_reported(...)` without needing the whole
// module public — with the `BackfillOutcome`/`BackfillStop` it answers with. ⚠ That caller was the
// GUI shell's orderflow backfill thread (`vike-app`'s, as this said until 2026-09-28), and it went
// with the shell's local feed plane on 2026-09-09 (#1727): today only
// `tests/binance_backfill_smoke.rs` calls it. That stop reason is the point: a REST
// error used to be indistinguishable from end-of-history, so an error-truncated symbol looked
// complete, and only a RETURNED cause lets a caller reopen its run-once spawn guard. The
// `()`-returning `agg_trades_backfill` that predated it discarded exactly that value; it is
// retired, and nothing ever called it.
//
// SP3 Task 4 adds `latest_agg_trade` alongside — a small helper the real-network smoke test
// (`tests/binance_backfill_smoke.rs`) uses to derive a real, always-current `before_id`/
// `earliest_timestamp` pair; nothing in production calls it.
pub use feed::tick::trades::{
    BackfillOutcome, BackfillStop, agg_trades_backfill_reported, latest_agg_trade,
};

// vike-catalog `CatalogProvider` impl (crate-reorg catalog workstream): reachable as
// `vike_binance::CatalogProviderBinance` — the sole catalog path since Task 12 retired the pre-#269
// `SymbolInfo`/`fetch_spot_catalog` code.
#[cfg(feature = "execution")]
pub use account::identity::{
    BinanceKeyProbe, KeyPermissionProbeBinance, key_permission_probe, parse_api_restrictions,
};
pub use catalog::CatalogProviderBinance;
#[cfg(feature = "execution")]
pub use execution::fetch_binance_properties;
#[cfg(feature = "execution")]
pub use reconcile::{ReconcileClientBinance, recon_client};

/// This adapter's DECLARED static capability row (audit br6). The values live once in
/// [`vike_venues::venue_caps`]; re-exported here so `vike_binance::CAPABILITIES` is discoverable next to the
/// adapter and the test below ties it to the adapter's real behavior.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::BINANCE;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry_and_reality() {
        // `capabilities` from the non-const registry fn so the property asserts aren't const-folded
        // (clippy::assertions_on_constants); the eq pins the re-export to it.
        let capabilities = vike_venues::capabilities_for("binance");
        assert_eq!(super::CAPABILITIES, capabilities);
        // reality: `VenueRestBinancePerp` overrides `VenueRest::{modify_order, submit_batch,
        // cancel_batch}` with native fapi endpoints (execution/rest/perp.rs), so both flags are TRUE; it also
        // builds `reduceOnly` on every order.
        assert!(capabilities.supports_modify);
        assert!(capabilities.supports_native_batch);
        assert!(capabilities.supports_reduce_only);
        // Expanded axes (w2-task-5), tied to the builders:
        // - `"stop"` and `"stop_limit"`: perp CONDITIONAL `STOP_MARKET` / `STOP` on the Algo Service
        //   (`execution::rest::algo::build_perp_algo_params`: `POST /fapi/v1/algoOrder`; the venue refuses both on
        //   `/fapi/v1/order` since 2025-12-09), spot `STOP_LOSS` / `STOP_LOSS_LIMIT`
        //   (`execution::rest::spot::VenueRestBinanceSpot::build_order_params`); `stop_limit_kind_is_declared_and_built`
        //   below drives both.
        // - `"take_profit"` and the `TakeProfit` trigger: perp `TAKE_PROFIT_MARKET` / `TAKE_PROFIT` on
        //   the same Algo Service, spot `TAKE_PROFIT` / `TAKE_PROFIT_LIMIT`;
        //   `take_profit_and_iceberg_are_built_on_their_lanes` below drives both.
        // - `supports_display_quantity`: SPOT's `icebergQty`; the perpetual has none and refuses a
        //   `display_qty` by name (`execution::rest::perp::deny_unbuilt_kind`), as spot refuses GTD under the lane union.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(
            capabilities.supported_order_kinds,
            &["market", "limit", "stop", "stop_limit", "take_profit"]
        );
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        // - `"trailing"` by a PERCENT only (the attributes wave, smoke green 2026-11-09 on both lanes): spot
        //   `STOP_LOSS` / `TAKE_PROFIT` with `trailingDelta`, perp `TRAILING_STOP_MARKET` with `callbackRate`;
        //   the price trail and `trailing_limit` stay refused.
        assert_eq!(capabilities.percent_trail_kinds, &["trailing"]);
        assert!(!capabilities.supported_order_kinds.contains(&"trailing"));
        assert!(capabilities.supports_display_quantity);
        // TIF: GTC/IOC/FOK honored 1:1; the perp lane additionally wires native fapi GTD, so it is
        // in the core-preflight ADMIT set (lane union) but NOT in the honored/offer set (the
        // single "binance" key can't distinguish lanes; spot denies GTD).
        assert_eq!(
            capabilities.supported_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly]
        );
        assert!(capabilities.accepted_tifs.contains(&TimeInForce::Gtd));
        // margin: the order builder never reads `margin_mode` (account default cross). `max_batch`
        // = the fapi batchOrders chunk cap.
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 5);
        // post-only: perp `timeInForce=GTX`, spot `type=LIMIT_MAKER` (both lanes, so the intersection)
        assert!(capabilities.supports_post_only);
    }

    /// **The REAL tie for `"stop_limit"`** (the pattern of
    /// `crates/bridges/hyperliquid/src/execution/execution_tests.rs`'s `stop_limit_kind_is_declared_and_built`): the
    /// row declares the kind and the core edge admits it, AND the same request through each lane's real
    /// `submit_order` reaches the wire as that lane's stop-limit, over a scripted venue that panics on
    /// any request it does not name. Also pinned: a perp stop-limit modify sends nothing, a plain spot
    /// stop is `STOP_LOSS`, and a kind neither lane builds (`market_to_limit`) and a non-GTC stop-limit
    /// are refused by name with nothing sent.
    #[cfg(feature = "execution")]
    #[test]
    fn stop_limit_kind_is_declared_and_built() {
        use crate::account::modes::modes_tests::{Venue, perp_rest};
        use crate::execution::rest::spot::VenueRestBinanceSpot;
        use crate::family::signer::BinanceHmacSigner;
        use serde_json::json;
        use vike_bridge_core::rest::VenueRest;
        use vike_model::events::Event;
        use vike_model::{OrderRequest, OrderType, SymbolProperties, TimeInForce};

        let want = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
            rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let accepted = |events: &[Event]| match events {
            [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
                a.venue_order_id.as_deref().map(str::to_string)
            }
            _ => None,
        };
        let refused = |events: &[Event]| match events {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => Some(r.reason.to_string()),
            _ => None,
        };

        let perp_request = OrderRequest {
            order_client_id: "vtsl1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: -1,
            qty: 0.002,
            order_type: OrderType::StopLimit,
            price: Some(57_899.97),
            trigger_price: Some(57_999.94),
            reduce_only: true,
            ..Default::default()
        };

        // 1. PERP: the Algo Service `STOP`, the limit in `price`, GTC; never `/fapi/v1/order`.
        let venue = Venue::new().on(
            "POST",
            crate::execution::rest::algo::PATH_ALGO_ORDER,
            Ok(json!({"algoId": 4242})),
        );
        let mut perp =
            perp_rest(venue, crate::account::modes::modes_tests::DEMO_USDM, "BTCUSDT", 2.0);
        perp.properties =
            SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() };
        assert_eq!(accepted(&perp.submit_order(&perp_request)).as_deref(), Some("4242"));
        assert_eq!(
            perp.transport.params_of("POST", crate::execution::rest::algo::PATH_ALGO_ORDER),
            want(&[
                ("algoType", "CONDITIONAL"),
                ("symbol", "BTCUSDT"),
                ("side", "SELL"),
                ("type", "STOP"),
                ("quantity", "0.002"),
                ("clientAlgoId", "vtsl1"),
                ("newOrderRespType", "ACK"),
                ("positionSide", "BOTH"),
                ("reduceOnly", "true"),
                ("triggerPrice", "57999.9"),
                ("price", "57899.9"),
                ("timeInForce", "GTC"),
            ])
        );
        // an untriggered algo order is not modifiable: the advisory, and nothing sent
        let modified = perp.modify_order(&perp_request, None, Some(57_800.0));
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
        let unbuilt = OrderRequest {
            order_type: OrderType::MarketToLimit,
            price: None,
            trigger_price: None,
            ..perp_request.clone()
        };
        let why = refused(&perp.submit_order(&unbuilt)).expect("market_to_limit is refused");
        assert!(why.contains("builds no `market_to_limit` order"), "{why}");
        let ioc = OrderRequest { time_in_force: TimeInForce::Ioc, ..perp_request.clone() };
        let why = refused(&perp.submit_order(&ioc)).expect("a non-GTC stop-limit is refused");
        assert!(why.contains("GTC only (`ioc` was asked)"), "{why}");
        let priceless = OrderRequest { price: None, ..perp_request.clone() };
        assert!(refused(&perp.submit_order(&priceless)).is_some(), "a stop-limit needs its limit");
        // What binance cannot hold is a capability; an order's own missing terms a check before the wire.
        let refusal_id = |events: Vec<Event>| match events.as_slice() {
            [_, Event::OrderRejected(r)] => r.refusal_id.as_deref().map(str::to_string),
            other => panic!("{other:?}"),
        };
        let capability = Some(vike_model::OrderRefusalId::VenueCapability.word().to_string());
        assert_eq!(refusal_id(perp.submit_order(&unbuilt)), capability);
        assert_eq!(refusal_id(perp.submit_order(&ioc)), capability);
        let not_sent = Some(vike_model::OrderRefusalId::NotSent.word().to_string());
        assert_eq!(refusal_id(perp.submit_order(&priceless)), not_sent);
        assert_eq!(perp.transport.sent(), ["POST /fapi/v1/algoOrder"], "one request in all");

        // 2. SPOT: `STOP_LOSS_LIMIT` with `price`, GTC and `stopPrice`; a plain stop is `STOP_LOSS`.
        let creds = vike_secrets::credentials::Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let spot = || VenueRestBinanceSpot {
            signer: BinanceHmacSigner::new(&creds, || 0),
            transport: Venue::new().on(
                "POST",
                crate::execution::rest::spot::PATH_ORDER,
                Ok(json!({"orderId": 77})),
            ),
            base_url: "https://stub".to_string(),
            symbol: "ETHUSDT".to_string(),
            properties: SymbolProperties {
                tick_size: 0.01,
                step_size: 0.0001,
                ..Default::default()
            },
            base_asset: "ETH".to_string(),
            link_id: None,
        };
        let spot_request = OrderRequest {
            order_client_id: "vtsl2".into(),
            symbol: "ETHUSDT".into(),
            side: 1,
            qty: 0.0042,
            price: Some(3_120.019),
            trigger_price: Some(3_100.005),
            reduce_only: false,
            ..perp_request.clone()
        };
        let base = [
            ("symbol", "ETHUSDT"),
            ("side", "BUY"),
            ("type", "STOP_LOSS_LIMIT"),
            ("quantity", "0.0042"),
            ("newClientOrderId", "vtsl2"),
            ("newOrderRespType", "ACK"),
        ];
        let client = spot();
        assert_eq!(accepted(&client.submit_order(&spot_request)).as_deref(), Some("77"));
        let mut rows = base.to_vec();
        rows.extend([("timeInForce", "GTC"), ("price", "3120.01"), ("stopPrice", "3100.00")]);
        assert_eq!(
            client.transport.params_of("POST", crate::execution::rest::spot::PATH_ORDER),
            want(&rows)
        );

        let client = spot();
        let stop =
            OrderRequest { order_type: OrderType::Stop, price: None, ..spot_request.clone() };
        assert_eq!(accepted(&client.submit_order(&stop)).as_deref(), Some("77"));
        let mut rows = base.to_vec();
        rows[2] = ("type", "STOP_LOSS");
        rows.push(("stopPrice", "3100.00"));
        assert_eq!(
            client.transport.params_of("POST", crate::execution::rest::spot::PATH_ORDER),
            want(&rows)
        );
        let client = spot();
        let unbuilt =
            OrderRequest { order_type: OrderType::MarketToLimit, price: None, ..spot_request };
        assert!(refused(&client.submit_order(&unbuilt)).is_some(), "refused on spot too");
        assert!(client.transport.sent().is_empty(), "and nothing was sent");

        // 3. The DECLARATION that what was built is admitted: the row lists it, the core edge admits it.
        // (Last, so the two wires above are proven on a tree whose row has not caught up yet.)
        let capabilities = vike_venues::capabilities_for("binance");
        assert!(capabilities.supported_order_kinds.contains(&"stop_limit"), "{capabilities:?}");
        assert!(
            capabilities.reduce_only_kinds.contains(&"stop_limit"),
            "a protective exit is reduce-only"
        );
        assert_eq!(vike_venues::preflight_order(&perp_request), Ok(()), "core edge must admit it");
    }

    /// A REDUCE-ONLY stop-limit (a protective exit: a SELL under the market) through the builders this
    /// bridge really uses for it, the exact parameters: the perp Algo Service `STOP`
    /// (`execution::rest::algo::build_perp_algo_params`, `reduceOnly=true`) and the spot override
    /// (`execution::rest::spot::VenueRestBinanceSpot::build_order_params`, `STOP_LOSS_LIMIT`; spot has no reduce-only flag,
    /// so none is sent, as for every spot kind). Not the shared family builder, which binance does not
    /// call for a stop-limit (it would send `MARKET`, decision 0186).
    #[cfg(feature = "execution")]
    #[test]
    fn a_reduce_only_stop_limit_builds_the_exact_params_on_both_lanes() {
        use crate::execution::rest::spot::VenueRestBinanceSpot;
        use crate::family::signer::BinanceHmacSigner;
        use vike_model::{OrderRequest, OrderType, SymbolProperties, TriggerBy};

        let exit = OrderRequest {
            order_client_id: "vtslX1".into(),
            venue: "binance".into(),
            side: -1,
            qty: 0.0105,
            order_type: OrderType::StopLimit,
            trigger_price: Some(2_011.867),
            price: Some(2_011.761),
            reduce_only: true,
            trigger_by: Some(TriggerBy::Mark),
            ..Default::default()
        };
        let pairs = |p: Vec<(&'static str, String)>| -> Vec<(String, String)> {
            p.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
        };
        let want = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
            rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let grid = SymbolProperties { tick_size: 0.01, step_size: 0.001, ..Default::default() };
        let perp = crate::execution::rest::algo::build_perp_algo_params(
            &exit,
            "ETHUSDT",
            &grid,
            crate::execution::rest::perp::TIME_IN_FORCE_LANE,
            Some("LINK"),
        );
        assert_eq!(
            pairs(perp),
            want(&[
                ("algoType", "CONDITIONAL"),
                ("symbol", "ETHUSDT"),
                ("side", "SELL"),
                ("type", "STOP"),
                ("quantity", "0.010"),
                ("clientAlgoId", "x-LINK-vtslX1"),
                ("newOrderRespType", "ACK"),
                ("positionSide", "BOTH"),
                ("reduceOnly", "true"),
                ("triggerPrice", "2011.86"),
                ("price", "2011.76"),
                ("timeInForce", "GTC"),
                ("workingType", "MARK_PRICE"),
            ])
        );

        let creds = vike_secrets::credentials::Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let spot = VenueRestBinanceSpot {
            signer: BinanceHmacSigner::new(&creds, || 0),
            transport: crate::account::modes::modes_tests::Venue::new(),
            base_url: "https://stub".to_string(),
            symbol: "ETHUSDT".to_string(),
            properties: SymbolProperties { step_size: 0.0001, ..grid },
            base_asset: "ETH".to_string(),
            link_id: Some("LINK".to_string()),
        };
        // spot evaluates on last trades only: a stop-limit asks for no source there
        let spot_exit = OrderRequest { trigger_by: None, ..exit };
        assert_eq!(
            pairs(spot.build_order_params(&spot_exit)),
            want(&[
                ("symbol", "ETHUSDT"),
                ("side", "SELL"),
                ("type", "STOP_LOSS_LIMIT"),
                ("quantity", "0.0105"),
                ("newClientOrderId", "x-LINK-vtslX1"),
                ("newOrderRespType", "ACK"),
                ("timeInForce", "GTC"),
                ("price", "2011.76"),
                ("stopPrice", "2011.86"),
            ])
        );
    }

    /// **The REAL tie for `"take_profit"` and the iceberg** (the stop-limit tie's pattern): each request
    /// through its lane's real `submit_order` over a scripted venue that panics on any request it does
    /// not name. PERP: the Algo Service `TAKE_PROFIT_MARKET`, and `TAKE_PROFIT` with its limit in `price`
    /// and GTC, never `/fapi/v1/order`, out of a batch too; a modify sends nothing; a non-GTC limit, a
    /// missing trigger and any `display_qty` are refused by name. SPOT: `TAKE_PROFIT` and
    /// `TAKE_PROFIT_LIMIT` with `stopPrice`, `icebergQty` on a GTC `TAKE_PROFIT_LIMIT` and a GTC `LIMIT`,
    /// and an iceberg on any other kind or lifetime refused with nothing sent.
    #[cfg(feature = "execution")]
    #[test]
    fn take_profit_and_iceberg_are_built_on_their_lanes() {
        use crate::account::modes::modes_tests::{Venue, perp_rest};
        use crate::execution::rest::algo::PATH_ALGO_ORDER;
        use crate::execution::rest::spot::{PATH_ORDER, VenueRestBinanceSpot};
        use crate::family::signer::BinanceHmacSigner;
        use serde_json::json;
        use vike_bridge_core::rest::VenueRest;
        use vike_model::events::Event;
        use vike_model::{OrderRequest, OrderType, SymbolProperties, TimeInForce};

        let want = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
            rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let accepted = |events: &[Event]| match events {
            [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
                a.venue_order_id.as_deref().map(str::to_string)
            }
            _ => None,
        };
        let refused = |events: &[Event]| match events {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => Some(r.reason.to_string()),
            _ => None,
        };

        // 1. PERP
        let perp_on = || {
            let venue = Venue::new().on("POST", PATH_ALGO_ORDER, Ok(json!({"algoId": 4343})));
            let mut perp =
                perp_rest(venue, crate::account::modes::modes_tests::DEMO_USDM, "BTCUSDT", 2.0);
            perp.properties =
                SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() };
            perp
        };
        let take_profit = OrderRequest {
            order_client_id: "vttp1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: -1,
            qty: 0.002,
            order_type: OrderType::TakeProfit,
            trigger_price: Some(64_000.06),
            reduce_only: true,
            ..Default::default()
        };
        let head = [
            ("algoType", "CONDITIONAL"),
            ("symbol", "BTCUSDT"),
            ("side", "SELL"),
            ("type", "TAKE_PROFIT_MARKET"),
            ("quantity", "0.002"),
            ("clientAlgoId", "vttp1"),
            ("newOrderRespType", "ACK"),
            ("positionSide", "BOTH"),
            ("reduceOnly", "true"),
            ("triggerPrice", "64000.0"),
        ];
        let perp = perp_on();
        assert_eq!(accepted(&perp.submit_order(&take_profit)).as_deref(), Some("4343"));
        assert_eq!(perp.transport.params_of("POST", PATH_ALGO_ORDER), want(&head));
        let perp = perp_on();
        let _ = perp.submit_batch(std::slice::from_ref(&take_profit));
        assert_eq!(perp.transport.sent(), ["POST /fapi/v1/algoOrder"], "a batch sends it alone");

        let take_profit_limit = OrderRequest { price: Some(64_010.04), ..take_profit.clone() };
        let perp = perp_on();
        assert_eq!(accepted(&perp.submit_order(&take_profit_limit)).as_deref(), Some("4343"));
        let mut rows = head.to_vec();
        rows[3] = ("type", "TAKE_PROFIT");
        rows.extend([("price", "64010.0"), ("timeInForce", "GTC")]);
        assert_eq!(perp.transport.params_of("POST", PATH_ALGO_ORDER), want(&rows));
        let modified = perp.modify_order(&take_profit_limit, None, Some(64_020.0));
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
        let ioc = OrderRequest { time_in_force: TimeInForce::Ioc, ..take_profit_limit.clone() };
        let why = refused(&perp.submit_order(&ioc)).expect("a non-GTC take-profit limit");
        assert!(why.contains("take_profit with a limit GTC only (`ioc` was asked)"), "{why}");
        let iceberg = OrderRequest { display_quantity: Some(0.001), ..take_profit_limit.clone() };
        let why = refused(&perp.submit_order(&iceberg)).expect("the perpetual has no iceberg");
        assert!(why.contains("perpetual has no iceberg"), "{why}");
        let triggerless = OrderRequest { trigger_price: None, ..take_profit.clone() };
        assert!(
            refused(&perp.submit_order(&triggerless)).is_some(),
            "a take-profit needs a trigger"
        );
        assert_eq!(perp.transport.sent(), ["POST /fapi/v1/algoOrder"], "one request in all");

        // 2. SPOT
        let creds = vike_secrets::credentials::Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let spot = || VenueRestBinanceSpot {
            signer: BinanceHmacSigner::new(&creds, || 0),
            transport: Venue::new().on("POST", PATH_ORDER, Ok(json!({"orderId": 77}))),
            base_url: "https://stub".to_string(),
            symbol: "ETHUSDT".to_string(),
            properties: SymbolProperties {
                tick_size: 0.01,
                step_size: 0.0001,
                ..Default::default()
            },
            base_asset: "ETH".to_string(),
            link_id: None,
        };
        let spot_take_profit = OrderRequest {
            order_client_id: "vttp2".into(),
            symbol: "ETHUSDT".into(),
            side: 1,
            qty: 0.0042,
            trigger_price: Some(2_900.005),
            reduce_only: false,
            ..take_profit.clone()
        };
        let base = [
            ("symbol", "ETHUSDT"),
            ("side", "BUY"),
            ("type", "TAKE_PROFIT"),
            ("quantity", "0.0042"),
            ("newClientOrderId", "vttp2"),
            ("newOrderRespType", "ACK"),
        ];
        let sent_for = |req: &OrderRequest| {
            let client = spot();
            assert_eq!(accepted(&client.submit_order(req)).as_deref(), Some("77"), "{req:?}");
            client.transport.params_of("POST", PATH_ORDER)
        };
        let mut rows = base.to_vec();
        rows.push(("stopPrice", "2900.00"));
        assert_eq!(sent_for(&spot_take_profit), want(&rows));

        let spot_take_profit_limit = OrderRequest {
            price: Some(2_905.019),
            display_quantity: Some(0.00155),
            ..spot_take_profit.clone()
        };
        let mut rows = base.to_vec();
        rows[2] = ("type", "TAKE_PROFIT_LIMIT");
        rows.extend([
            ("timeInForce", "GTC"),
            ("price", "2905.01"),
            ("stopPrice", "2900.00"),
            ("icebergQty", "0.0015"),
        ]);
        assert_eq!(sent_for(&spot_take_profit_limit), want(&rows));

        let iceberg_limit = OrderRequest {
            order_type: OrderType::Limit,
            trigger_price: None,
            price: Some(2_905.019),
            display_quantity: Some(0.001),
            ..spot_take_profit.clone()
        };
        let mut rows = base.to_vec();
        rows[2] = ("type", "LIMIT");
        rows.extend([("timeInForce", "GTC"), ("price", "2905.01"), ("icebergQty", "0.0010")]);
        assert_eq!(sent_for(&iceberg_limit), want(&rows));

        // an iceberg the venue does not take, and a take-profit limit asking another lifetime
        let client = spot();
        for bad in [
            OrderRequest { display_quantity: Some(0.001), ..spot_take_profit.clone() },
            OrderRequest { time_in_force: TimeInForce::Ioc, ..iceberg_limit.clone() },
            OrderRequest { time_in_force: TimeInForce::PostOnly, ..iceberg_limit.clone() },
            OrderRequest { order_type: OrderType::Market, price: None, ..iceberg_limit.clone() },
            OrderRequest { display_quantity: Some(0.0), ..iceberg_limit.clone() },
            OrderRequest { display_quantity: Some(f64::NAN), ..iceberg_limit.clone() },
            OrderRequest { time_in_force: TimeInForce::Fok, ..spot_take_profit_limit.clone() },
        ] {
            assert!(refused(&client.submit_order(&bad)).is_some(), "{bad:?}");
        }
        let modified = client.modify_order(&spot_take_profit_limit, None, Some(2_906.0));
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
        assert!(client.transport.sent().is_empty(), "nothing was sent");
    }

    /// **The REAL tie for a PERCENT `"trailing"`** (the stop-limit tie's pattern), which the row lists
    /// (`percent_trail_kinds`, opened behind `tests/binance_smokes/trailing_percent_smoke.rs`, green 2026-11-09):
    /// each request through its lane's real `submit_order` over a scripted venue that panics on any request
    /// it does not name. PERP: the Algo Service `TRAILING_STOP_MARKET`, the percent as `callbackRate` and an
    /// activation as `activatePrice`, never `/fapi/v1/order`, out of a batch too; a modify sends nothing; a
    /// COIN-M client refuses it. SPOT: `STOP_LOSS` with `trailingDelta` in whole basis points, and
    /// `TAKE_PROFIT` with the activation as `stopPrice` on either side (the trailing-stop FAQ's side table),
    /// and a delta under the uncached `TRAILING_DELTA` filter left to the venue. Both: a price trail, a
    /// `trailing_limit`, an iceberg, bad terms, (perp) a percent outside 0.1 to 20 and (spot) a percent
    /// that is no whole basis point are refused by name with nothing sent.
    #[cfg(feature = "execution")]
    #[test]
    fn a_percent_trailing_stop_is_built_on_both_lanes() {
        use crate::account::modes::modes_tests::{Venue, perp_rest};
        use crate::execution::rest::algo::PATH_ALGO_ORDER;
        use crate::execution::rest::spot::{PATH_ORDER, VenueRestBinanceSpot, trailing_delta_bips};
        use crate::family::signer::BinanceHmacSigner;
        use serde_json::json;
        use vike_bridge_core::rest::VenueRest;
        use vike_model::events::Event;
        use vike_model::{OrderRequest, OrderType, SymbolProperties};

        let want = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
            rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let accepted = |events: &[Event]| match events {
            [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
                a.venue_order_id.as_deref().map(str::to_string)
            }
            _ => None,
        };
        let refused = |events: &[Event]| match events {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => Some(r.reason.to_string()),
            _ => None,
        };

        // 1. PERP
        let perp_on = || {
            let venue = Venue::new().on("POST", PATH_ALGO_ORDER, Ok(json!({"algoId": 4545})));
            let mut perp =
                perp_rest(venue, crate::account::modes::modes_tests::DEMO_USDM, "BTCUSDT", 2.0);
            perp.properties =
                SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() };
            perp
        };
        let trail = OrderRequest {
            order_client_id: "vttr1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: -1,
            qty: 0.002,
            order_type: OrderType::Trailing,
            trail_percent: Some(1.5),
            reduce_only: true,
            ..Default::default()
        };
        let head = [
            ("algoType", "CONDITIONAL"),
            ("symbol", "BTCUSDT"),
            ("side", "SELL"),
            ("type", "TRAILING_STOP_MARKET"),
            ("quantity", "0.002"),
            ("clientAlgoId", "vttr1"),
            ("newOrderRespType", "ACK"),
            ("positionSide", "BOTH"),
            ("reduceOnly", "true"),
            ("callbackRate", "1.5"),
        ];
        let perp = perp_on();
        assert_eq!(accepted(&perp.submit_order(&trail)).as_deref(), Some("4545"));
        assert_eq!(perp.transport.params_of("POST", PATH_ALGO_ORDER), want(&head));
        let armed = OrderRequest { trigger_price: Some(66_000.04), ..trail.clone() };
        let perp = perp_on();
        assert_eq!(accepted(&perp.submit_order(&armed)).as_deref(), Some("4545"));
        let mut rows = head.to_vec();
        rows.push(("activatePrice", "66000.0"));
        assert_eq!(perp.transport.params_of("POST", PATH_ALGO_ORDER), want(&rows));
        let modified = perp.modify_order(&armed, None, None);
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
        assert_eq!(perp.transport.sent(), ["POST /fapi/v1/algoOrder"], "the modify sent nothing");
        let perp = perp_on();
        let _ = perp.submit_batch(std::slice::from_ref(&trail));
        assert_eq!(perp.transport.sent(), ["POST /fapi/v1/algoOrder"], "a batch sends it alone");
        let perp = perp_on();
        for (bad, words) in [
            (
                OrderRequest { trail_percent: None, trail: Some(50.0), ..trail.clone() },
                "never by a price",
            ),
            (
                OrderRequest {
                    order_type: OrderType::TrailingLimit,
                    limit_offset: Some(5.0),
                    ..trail.clone()
                },
                "builds no `trailing_limit` order",
            ),
            (
                OrderRequest { display_quantity: Some(0.001), ..trail.clone() },
                "perpetual has no iceberg",
            ),
            (OrderRequest { trail_percent: None, ..trail.clone() }, "needs a trail distance"),
            (OrderRequest { price: Some(60_000.0), ..trail.clone() }, "takes no price"),
            // the New Algo Order page's `callbackRate` range, 0.1 to 20, refused outside it locally
            (OrderRequest { trail_percent: Some(20.5), ..trail.clone() }, "0.1% to 20%"),
            (OrderRequest { trail_percent: Some(0.05), ..trail.clone() }, "0.1% to 20%"),
        ] {
            let why = refused(&perp.submit_order(&bad)).unwrap_or_else(|| panic!("{bad:?}"));
            assert!(why.contains(words), "{why}");
        }
        assert!(perp.transport.sent().is_empty(), "nothing was sent");
        for edge in [0.1, 20.0] {
            let perp = perp_on();
            let req = OrderRequest { trail_percent: Some(edge), ..trail.clone() };
            assert!(accepted(&perp.submit_order(&req)).is_some(), "{edge}% is inside the range");
        }
        // A COIN-M client (its book, never its host, says so).
        let coin_m =
            crate::execution::rest::coin_m::deny_unbuilt(vike_model::Margining::Inverse, &trail);
        assert!(coin_m.is_some(), "dapi has no Algo Service");

        // 2. SPOT
        for (percent, bips) in [
            (1.5, Some(150)),
            (0.1, Some(10)),
            (1.23, Some(123)),
            (0.01, Some(1)),
            (99.99, Some(9999)),
            (1.234, None),
            (0.005, None),
        ] {
            assert_eq!(trailing_delta_bips(percent), bips, "{percent}");
        }
        let creds = vike_secrets::credentials::Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let spot = || VenueRestBinanceSpot {
            signer: BinanceHmacSigner::new(&creds, || 0),
            transport: Venue::new().on("POST", PATH_ORDER, Ok(json!({"orderId": 78}))),
            base_url: "https://stub".to_string(),
            symbol: "ETHUSDT".to_string(),
            properties: SymbolProperties {
                tick_size: 0.01,
                step_size: 0.0001,
                ..Default::default()
            },
            base_asset: "ETH".to_string(),
            link_id: None,
        };
        let spot_trail = OrderRequest {
            order_client_id: "vttr2".into(),
            symbol: "ETHUSDT".into(),
            side: 1,
            qty: 0.0042,
            trail_percent: Some(1.25),
            reduce_only: false,
            ..trail.clone()
        };
        let base = [
            ("symbol", "ETHUSDT"),
            ("side", "BUY"),
            ("type", "STOP_LOSS"),
            ("quantity", "0.0042"),
            ("newClientOrderId", "vttr2"),
            ("newOrderRespType", "ACK"),
        ];
        let sent_for = |req: &OrderRequest| {
            let client = spot();
            assert_eq!(accepted(&client.submit_order(req)).as_deref(), Some("78"), "{req:?}");
            client.transport.params_of("POST", PATH_ORDER)
        };
        let mut rows = base.to_vec();
        rows.push(("trailingDelta", "125"));
        assert_eq!(sent_for(&spot_trail), want(&rows));
        let spot_armed = OrderRequest { trigger_price: Some(2_900.005), ..spot_trail.clone() };
        let mut rows = base.to_vec();
        rows[2] = ("type", "TAKE_PROFIT");
        rows.extend([("stopPrice", "2900.00"), ("trailingDelta", "125")]);
        assert_eq!(sent_for(&spot_armed), want(&rows));
        // a SELL armed above the market is the same word (the FAQ: a SELL `TAKE_PROFIT` arms on a rise)
        let sell_armed =
            OrderRequest { side: -1, trigger_price: Some(3_300.0), ..spot_armed.clone() };
        rows[1] = ("side", "SELL");
        rows[6] = ("stopPrice", "3300.00");
        assert_eq!(sent_for(&sell_armed), want(&rows));
        // the symbol's `TRAILING_DELTA` filter is not cached here: a whole delta under it goes out and
        // the venue judges it
        let tiny = OrderRequest { trail_percent: Some(0.05), ..spot_trail.clone() };
        let mut rows = base.to_vec();
        rows.push(("trailingDelta", "5"));
        assert_eq!(sent_for(&tiny), want(&rows));
        let client = spot();
        for (bad, words) in [
            (
                OrderRequest { trail_percent: Some(1.234), ..spot_trail.clone() },
                "whole basis points",
            ),
            (
                OrderRequest { trail_percent: None, trail: Some(5.0), ..spot_trail.clone() },
                "never by a price",
            ),
            (
                OrderRequest { display_quantity: Some(0.001), ..spot_trail.clone() },
                "binance spot shows part",
            ),
            (
                OrderRequest {
                    order_type: OrderType::TrailingLimit,
                    limit_offset: Some(1.0),
                    ..spot_trail.clone()
                },
                "builds no `trailing_limit` order",
            ),
        ] {
            let why = refused(&client.submit_order(&bad)).unwrap_or_else(|| panic!("{bad:?}"));
            assert!(why.contains(words), "{why}");
        }
        let modified = client.modify_order(&spot_armed, None, None);
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
        assert!(client.transport.sent().is_empty(), "nothing was sent");
    }

    /// **The DECLARATION of what `take_profit_and_iceberg_are_built_on_their_lanes` built**: the row lists
    /// `take_profit`, reduce-only too (the perp algo carries `reduceOnly`; spot sends no flag on any kind,
    /// as for the stop-limit), declares the take-profit trigger and the visible size, and the core edge
    /// admits a take-profit and an iceberg.
    #[test]
    fn the_row_declares_the_take_profit_and_the_iceberg() {
        use vike_model::{OrderRequest, OrderType};
        use vike_venues::TriggerType;
        let capabilities = vike_venues::capabilities_for("binance");
        assert!(capabilities.supported_order_kinds.contains(&"take_profit"), "{capabilities:?}");
        assert!(
            capabilities.reduce_only_kinds.contains(&"take_profit"),
            "a protective exit is reduce-only"
        );
        assert!(capabilities.trigger_types.contains(&TriggerType::TakeProfit), "{capabilities:?}");
        assert!(capabilities.supports_display_quantity, "{capabilities:?}");
        let take_profit = OrderRequest {
            order_client_id: "vttp3".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: -1,
            qty: 0.002,
            order_type: OrderType::TakeProfit,
            trigger_price: Some(64_000.0),
            price: Some(63_990.0),
            reduce_only: true,
            ..Default::default()
        };
        assert_eq!(vike_venues::preflight_order(&take_profit), Ok(()), "core edge must admit it");
        let iceberg = OrderRequest {
            symbol: "ETHUSDT".into(),
            order_type: OrderType::Limit,
            trigger_price: None,
            reduce_only: false,
            display_quantity: Some(0.001),
            ..take_profit
        };
        assert_eq!(vike_venues::preflight_order(&iceberg), Ok(()), "core edge must admit it");
    }
}
