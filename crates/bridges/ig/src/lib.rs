//! IG (IG Group) FX/CFD adapter — stateful session-auth REST.
//!
//! DATA half: config + login ([`IgSession`]) + prices (→ `Bar`).
//! EXEC half: [`spawn_execution_client`] — positions (`/positions/otc`) + working orders
//! (`/workingorders/otc`) resolved via `/confirms/{dealRef}`, plus the async fill/terminal lane
//! ([`execution::user_data`] — the Lightstreamer trade-update stream, wired into `execution/mod.rs`) for delayed
//! working-order fills. A `reduce_only` MARKET order CLOSES (`/positions/otc` + `_method: DELETE`)
//! rather than opening an opposing deal — `exec`'s module doc is the authority on that routing.
//!
//! MARKET-DATA half: REST history ([`history::prices`]) plus the LIVE feed — the shared data engine over
//! [`feed::DataClientSpecIg`], a [`vike_data::DataClient`] over IG's Lightstreamer TLCP
//! protocol spoken raw on the shared tungstenite WS stack. Its pure halves are [`feed::decode::lightstreamer`] (the TLCP codec) and
//! [`feed::decode`] (the MARKET/CHART field normalizers).
//!
//! HISTORY half: [`KlineSourceIg`] (the store-free source) and [`credentialed_klines`] (the demo account's
//! read, the allowance ledger and the spec the gatewayhub's credentialed kline engine serves the lane
//! through).

#![warn(unreachable_pub)]

pub mod adapter;
mod catalog;
mod config;
// The history lane's demo-account read, its allowance ledger and the spec the gatewayhub's credentialed
// kline engine runs it through.
pub mod credentialed_klines;
pub mod symbol_properties;

pub(crate) mod account;
// The epic of a USER name and the user name of an epic, off the KEPT row (decision 0290).
mod epic;
mod execution;
pub mod feed;
pub(crate) mod history;
mod instrument;
// IG's market list (`GET /categories` and its pages) over a held session: the gatewayhub's ig catalog entry.
pub mod market_list;
// One instrument the operator adds by hand, looked up over a held session (the datahub's `OperatorInstruments`).
pub mod operator_row;
pub mod reconcile;
// Every refusal this bridge answers with, one id and one text each (`vike_utility::refusal_enum!`).
pub mod refusals;
// IG's half of the gatewayhub's per-key bounds on its symbol facts (the keyed spec).
mod transport;

pub use account::quote_rate::{QuoteRateReading, RATE_TOLERANCE};
pub use catalog::{CatalogProviderIg, parse_markets};
pub use config::{FIELDS, IgConfig, ig_rest_base, load_ig_config_for, load_ig_config_from};
pub use execution::{FallbackEpic, spawn_execution_client};
pub use history::klines::{
    Allowance, AllowanceGate, ConfigProvider, HistorySession, KlineSourceIg, LoginFn, PACE,
    PricesApi, REQUEST_SHARE_PERCENT, RESERVE_PERCENT, SESSION_IDLE, SessionCache, SessionLoginFn,
    chunk_words, estimate_points, exhausted_words, parse_allowance, request_cap_points,
    request_share_words, reserve_points, reserve_stop_words, reset_sentence, served_interval,
    span_words,
};
pub use history::prices::{fetch_prices, parse_ig_time_utc, parse_prices, resolution};
pub use instrument::{
    FACTS_LOOKUP_WAIT, IgProperties, IgPropertiesError, IgPropertiesSource, MAX_READS_PER_MINUTE,
    market_facts, mount_contract_value, quoted_market_facts,
};
pub use reconcile::{ReconcileClientIg, recon_client};
pub use transport::{IG_HTTP, IgSession};

/// Test-only surface: the pure `/confirms` accept/fill/reject mapper (`execution::map_confirm`) and the
/// Lightstreamer trade-update decode (`event_mapper::decode_trade_confirm`). Both modules stay
/// private; these items are `pub` only so this re-export can reach them, then re-exported here
/// `#[doc(hidden)]` so the cross-bridge conformance harness
/// (`vike-bridge-core/tests/bridge_conformance.rs`) can drive IG's REAL mappers through the shared
/// scenario table. Mirrors oanda's `#[doc(hidden)] pub use execution::map_order_response`. Not part of
/// the venue-adapter public API.
#[doc(hidden)]
pub use execution::event_mapper::decode_trade_confirm;
#[doc(hidden)]
pub use execution::map_confirm;

/// Test-only surface: how a submit resolves when IG's `/confirms` never answered. `pub` here so
/// `crates/bridges/ig/tests/offline/confirm_never_terminalizes.rs` can fold its output through the REAL
/// `vike_model::OrderRecord` FSM — the same oracle the cross-bridge conformance harness uses — and
/// assert the no-false-terminal contract where the decision actually lives. Not public API.
#[doc(hidden)]
pub use execution::unresolved_confirm;

/// Test-only surface (`test-support`): hold the ig kept list this crate's tests read, so the cross-bridge
/// conformance harness mounts ig on the USER name `EURUSD` (decision 0290) and its REAL mappers name the
/// fill by it. Not public API. Also on under `test`, so the item is reachable when this crate's own unit tests build
/// without the feature (`unreachable_pub` otherwise refuses a lone `cargo clippy -p vike-ig --all-targets`).
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use epic::hold_kept_list_for_tests;

/// Test-only surface: the two request BUILDERS, so `crates/bridges/ig/tests/ig_smokes/ig_close_position_smoke.rs`
/// opens and closes a real demo position through the SAME bytes production sends. A smoke that
/// hand-rolled its own bodies would prove IG's endpoint works and nothing about this bridge. Not
/// public API.
#[doc(hidden)]
pub use execution::{
    build_close_request, build_close_request_expiring, build_request, build_request_expiring,
};

/// This adapter's DECLARED static capability row. Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::IG;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("ig");
        assert_eq!(super::CAPABILITIES, capabilities);
        // `spawn_execution_client` wires submit/cancel (positions + working orders) and a PRICE-ONLY
        // modify (`PUT /workingorders/otc/{dealId}`, `execution::lifecycle::amend_working_order`); no
        // batch.
        assert!(capabilities.supports_modify);
        assert!(!capabilities.supports_native_batch);
        // ⚠ `supports_reduce_only` stays FALSE even though a `reduce_only` order now CLOSES here.
        // That field asks whether the adapter builds a reduce-only FLAG onto the WIRE order; IG has
        // no such field — `reduce_only` selects a different ENDPOINT instead (`exec`'s module doc
        // has the routing table). cTrader, which closes by position id, declares `false` for the
        // same reason. Flipping it would claim a wire field that does not exist.
        assert!(!capabilities.supports_reduce_only);
        // LIVE DATA: the client over `feed::DataClientSpecIg` serves bars + quotes over Lightstreamer and
        // NOTHING else — IG publishes a dealer L1 with no ladder behind it and no public tape.
        assert!(capabilities.has_live_data());
        assert_eq!(
            capabilities.live_data,
            vike_venues::venue_caps::DataClientCapabilities {
                bars: true,
                quotes: true,
                trades: false,
                book: false,
                depth: false
            }
        );
        // Expanded axes: `build_request` wires `"limit"`/`"stop"` as working orders
        // (level ← price/trigger_price) and `"market"` as a position open; every other kind is
        // refused by name (`execution/mod.rs`'s `unbuilt_kind`) → take_profit unwired. TIF is never read
        // (working orders hardcode GOOD_TILL_CANCELLED), so accepted == &[Gtc]: a non-GTC limit is preflight-refused instead of silently resting GTC
        // (the loud-deny flip this axis exists for). `margin_mode` never read; no batch.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(capabilities.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(capabilities.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(!capabilities.supports_post_only);
    }
}
