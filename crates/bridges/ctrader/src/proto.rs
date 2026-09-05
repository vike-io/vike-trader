//! Generated cTrader Open API protobuf types (prost) + the payloadType registry.
//! Ports nothing — mirrors the Spotware Open API wire schema (proto/ vendored, pinned).

// The `#![allow(clippy::all)]` is scoped to ONLY the machine-generated prost output (which trips
// many pedantic lints we cannot fix without editing generated code), NOT the hand-written `pt`
// module below — that stays under the crate's normal `-D warnings` gate. `pub use generated::*;`
// re-exports every generated type so `proto::ProtoMessage`/`proto::ProtoOa*` keep resolving.
//
// The bindings are PRE-GENERATED and committed (src/generated/openapi.rs) rather than produced at
// build time: this crate no longer runs prost-build/protoc, keeping them (and vendored protoc) out
// of the normal build + supply chain. The committed file is prost-build's verbatim output for the
// vendored proto/ schema; it is kept byte-identical to a fresh regen by the drift gate
// (.github/workflows/ctrader-proto.yml). prost emits into a file named after the proto `package`;
// cTrader uses no package, so prost's own filename is `_.rs` — renamed to `generated/openapi.rs`
// on commit. Regenerate with `cargo build -p vike-ctrader` (temporarily restoring build.rs) or,
// canonically, via the drift-gate workflow's regen step, and copy the emitted `_.rs` over it. The
// committed bytes stay prettyplease-formatted exactly as prost-build emits them: rustfmt only walks
// the `mod` tree, and this is an `include!` target (not a `mod`), so `cargo fmt` never visits it —
// the same reason the old `$OUT_DIR/_.rs` was never formatted.
mod generated {
    #![allow(clippy::all)]
    include!("generated/openapi.rs");
}
pub use generated::*;

/// payloadType ids we send/dispatch on (subset of ProtoOAPayloadType / ProtoPayloadType).
pub mod pt {
    pub const HEARTBEAT_EVENT: u32 = 51;
    pub const APPLICATION_AUTH_REQ: u32 = 2100;
    pub const APPLICATION_AUTH_RES: u32 = 2101;
    pub const ACCOUNT_AUTH_REQ: u32 = 2102;
    pub const ACCOUNT_AUTH_RES: u32 = 2103;
    pub const NEW_ORDER_REQ: u32 = 2106;
    pub const CANCEL_ORDER_REQ: u32 = 2108;
    pub const AMEND_ORDER_REQ: u32 = 2109;
    /// Close (or partially close) an existing position by its numeric `positionId` — the
    /// mode-agnostic flatten verb (`ProtoOAClosePositionReq`). Unlike an opposite-side
    /// `NEW_ORDER_REQ`, which on a HEDGING account opens an opposing hedged position instead of
    /// netting flat, this closes the specific tracked position; the venue answers with the same
    /// `EXECUTION_EVENT` stream a normal fill uses (a closing deal referencing the position id).
    pub const CLOSE_POSITION_REQ: u32 = 2111;
    pub const SYMBOLS_LIST_REQ: u32 = 2114;
    pub const SYMBOLS_LIST_RES: u32 = 2115;
    pub const SYMBOL_BY_ID_REQ: u32 = 2116;
    pub const SYMBOL_BY_ID_RES: u32 = 2117;
    pub const TRADER_REQ: u32 = 2121;
    pub const TRADER_RES: u32 = 2122;
    /// Ask the venue for the account's current OPEN positions + PENDING orders — issued on
    /// reconnect to rebuild the coid→orderId map and re-establish still-pending orders (F3).
    pub const RECONCILE_REQ: u32 = 2124;
    pub const RECONCILE_RES: u32 = 2125;
    pub const EXECUTION_EVENT: u32 = 2126;
    /// Historical fill (deal) query by timestamp range — the `ReconClient` fill-report fetch
    /// (ReconFactory seam, wave-2 task 6) rides this, not `EXECUTION_EVENT` (a live stream, not a
    /// query).
    pub const DEAL_LIST_REQ: u32 = 2133;
    pub const DEAL_LIST_RES: u32 = 2134;
    pub const SUBSCRIBE_SPOTS_REQ: u32 = 2127;
    pub const SUBSCRIBE_SPOTS_RES: u32 = 2128;
    pub const UNSUBSCRIBE_SPOTS_REQ: u32 = 2129;
    pub const UNSUBSCRIBE_SPOTS_RES: u32 = 2130;
    pub const SPOT_EVENT: u32 = 2131;
    /// Sent when an order request itself errors (vs. a lifecycle `EXECUTION_EVENT`) — carries an
    /// `orderId` we reverse-correlate to a `client_order_id` to synthesize a reject.
    pub const ORDER_ERROR_EVENT: u32 = 2132;
    pub const SUBSCRIBE_LIVE_TRENDBAR_REQ: u32 = 2135;
    pub const UNSUBSCRIBE_LIVE_TRENDBAR_REQ: u32 = 2136;
    pub const GET_TRENDBARS_REQ: u32 = 2137;
    pub const GET_TRENDBARS_RES: u32 = 2138;
    pub const SUBSCRIBE_LIVE_TRENDBAR_RES: u32 = 2165;
    pub const UNSUBSCRIBE_LIVE_TRENDBAR_RES: u32 = 2166;
    pub const GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ: u32 = 2149;
    pub const GET_ACCOUNTS_BY_ACCESS_TOKEN_RES: u32 = 2150;
    pub const ERROR_RES: u32 = 2142;
    pub const ACCOUNTS_TOKEN_INVALIDATED_EVENT: u32 = 2147;
}
