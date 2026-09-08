//! Hyperliquid L1 action wire structs — **field declaration order IS the signature**.
//!
//! These serialize (via [`rmp_serde::to_vec_named`] in [`super::hash::action_hash`]) to the exact
//! msgpack map the venue hashes, so every rename, every field order, and every "omit when absent"
//! rule below is load-bearing: a single differing byte recovers a *different* signer and the order
//! is silently rejected ("User or API Wallet 0x… does not exist" — research §11, landmine #6).
//! Ported field-for-field from the official Rust SDK (`src/exchange/{actions,order,cancel,
//! modify}.rs`) and cross-checked against the canonical Python SDK (`signing.py` /
//! `exchange.py`) — the two implementations that produce the golden vectors in
//! `tests/signing_vectors.rs`.
//!
//! Conventions that reproduce the wire byte-for-byte:
//! - [`Action`] is `#[serde(tag = "type", rename_all = "camelCase")]`, so the map emits `type`
//!   **first** (`"order"`/`"cancel"`/`"cancelByCloid"`/`"modify"`/`"batchModify"`) then the
//!   variant's fields.
//! - [`OrderWire`] renames its fields to the short forms **in order** `a, b, p, s, r, t, c`. `r`
//!   (reduceOnly) is ALWAYS present (a plain bool, both SDKs emit it even when `false`); only `c`
//!   (cloid) is `skip_serializing_if = "Option::is_none"` — absent, never `null`.
//! - [`OrderKind`] is externally tagged (`{"limit":{…}}` / `{"trigger":{…}}`); trigger fields are
//!   ordered `isMarket, triggerPx, tpsl`.
//! - [`OrderAction::builder`] is `skip_serializing_if = "Option::is_none"` — absent (not `null`) when
//!   unused, so a v1 order with no configured builder hashes BYTE-IDENTICALLY to before Builder Codes
//!   existed (this field has carried `None` since the crate's first commit, so every existing golden
//!   vector already proves the absent path). A `Some` [`HlBuilderFee`] rides at this ACTION level (one
//!   per batch, not per order — matches the real `/exchange` wire, unlike a per-`OrderWire` field).
//!
//! Note on the "omit `f`/`a` flags when false" landmine (research §6/§11, #3): the *current*
//! canonical HL `cancel`/`modify` wire carries **no** such flags — both SDKs define `cancel` items
//! as exactly `{a, o}` and `modify` as `{oid, order}` (that CCXT-ism was conflated in the research
//! note; the golden vectors here confirm the flag-free shape). None is emitted, which is the
//! required behaviour for the always-false v1 case. Should HL ever add an optional cancel/modify
//! flag, it MUST be `Option<_>` + `skip_serializing_if = "Option::is_none"` to preserve the hash.
//!
//! `p`/`s` are already-formatted canonical wire **strings** — [`crate::px`] produced them; carry
//! the string, never re-format it here (the emitted string is exactly what gets signed).

use serde::Serialize;

/// The signed L1 action envelope. `#[serde(tag = "type")]` puts the camelCased variant name first
/// in the msgpack map; each variant then flattens its inner struct's fields. Only the v1
/// (spot/perp trading) variants are modelled — leverage/transfer/etc. are deferred and, being
/// user-signed or additive, slot in later without reshaping this enum.
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    /// `{"type":"order", "orders":[…], "grouping":"na", "builder"?}` — HL is batch-first, so a
    /// single order is a one-element `orders`.
    Order(OrderAction),
    /// `{"type":"cancel", "cancels":[{"a":asset,"o":oid}]}` — cancel by exchange oid.
    Cancel(CancelAction),
    /// `{"type":"cancelByCloid", "cancels":[{"asset":…,"cloid":…}]}` — note the key rename
    /// `a/o → asset/cloid` between the two cancel variants (research §6).
    CancelByCloid(CancelByCloidAction),
    /// `{"type":"modify", "oid":…, "order":{…}}` — single cancel-replace (produces a NEW oid).
    Modify(ModifyWire),
    /// `{"type":"batchModify", "modifies":[{"oid":…,"order":{…}}]}` — the batched form.
    BatchModify(BatchModifyAction),
}

/// A single order in `orders`. Field order `a, b, p, s, r, t, c` is the signed msgpack order.
#[derive(Serialize, Clone, Debug)]
pub struct OrderWire {
    /// `a` — asset id (perp: `universe` index; spot: `10000 + spotIndex`; [`crate::symbology`]).
    #[serde(rename = "a")]
    pub asset: u32,
    /// `b` — is-buy.
    #[serde(rename = "b")]
    pub is_buy: bool,
    /// `p` — limit price, already canonical-wire-formatted ([`crate::px::float_to_wire`]).
    #[serde(rename = "p")]
    pub limit_px: String,
    /// `s` — size, already canonical-wire-formatted ([`crate::px::round_size`]).
    #[serde(rename = "s")]
    pub sz: String,
    /// `r` — reduce-only. ALWAYS serialized (both SDKs emit `false` explicitly).
    #[serde(rename = "r")]
    pub reduce_only: bool,
    /// `t` — order kind (`{"limit":…}` | `{"trigger":…}`).
    #[serde(rename = "t")]
    pub order_type: OrderKind,
    /// `c` — optional client order id (`0x` + 32 hex, 128-bit). Absent (not `null`) when `None`.
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub cloid: Option<String>,
}

/// The order-kind sub-object, externally tagged: `{"limit":{"tif":…}}` or
/// `{"trigger":{"isMarket":…,"triggerPx":…,"tpsl":…}}`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub enum OrderKind {
    /// A resting/aggressive limit order. `tif` is the case-sensitive `"Gtc" | "Ioc" | "Alo"`
    /// (post-only ⇒ `"Alo"`; an emulated market order ⇒ `"Ioc"` at an aggressive price).
    Limit(LimitParams),
    /// A stop/take-profit trigger order.
    Trigger(TriggerParams),
}

/// `{"tif": "Gtc"|"Ioc"|"Alo"}`. No rename — `tif` is already the wire key.
#[derive(Serialize, Clone, Debug)]
pub struct LimitParams {
    pub tif: String,
}

/// `{"isMarket":…, "triggerPx":…, "tpsl":…}` — field order is load-bearing.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TriggerParams {
    /// Fire as a market order when triggered (else a limit at `limit_px`).
    pub is_market: bool,
    /// Trigger price, canonical-wire-formatted.
    pub trigger_px: String,
    /// `"tp"` (take-profit) | `"sl"` (stop-loss).
    pub tpsl: String,
}

/// Builder-code fee attachment `{"b": address, "f": fee}` — Hyperliquid's per-order-flow
/// attribution mechanism (`vike_model::attribution::AttributionMechanic::SignedBuilder`, this
/// venue's row). `fee_tenths_bp` is the fee rate in tenths of a basis point (e.g. `10` = 0.1bp);
/// `0` is attribution-only (no fee charged, so the one-time on-chain `approveBuilderFee` —
/// [`crate::builder_fee::approve_builder_fee`] — is never required for that case).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct HlBuilderFee {
    /// `b` — builder address (lowercased `0x…`).
    #[serde(rename = "b")]
    pub address: String,
    /// `f` — fee in tenths of a basis point.
    #[serde(rename = "f")]
    pub fee_tenths_bp: u32,
}

/// The `order` action body: `{"orders":[…], "grouping":…, "builder"?}`.
#[derive(Serialize, Clone, Debug)]
pub struct OrderAction {
    /// One or more orders (HL is batch-first).
    pub orders: Vec<OrderWire>,
    /// `"na"` (no grouping) | `"normalTpsl"` | `"positionTpsl"` (attached TP/SL).
    pub grouping: String,
    /// Optional builder-fee code — absent (not `null`) when unused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builder: Option<HlBuilderFee>,
}

/// A single cancel-by-oid `{"a":asset, "o":oid}`.
#[derive(Serialize, Clone, Debug)]
pub struct CancelWire {
    #[serde(rename = "a")]
    pub asset: u32,
    #[serde(rename = "o")]
    pub oid: u64,
}

/// The `cancel` action body: `{"cancels":[{"a":…,"o":…}]}`.
#[derive(Serialize, Clone, Debug)]
pub struct CancelAction {
    pub cancels: Vec<CancelWire>,
}

/// A single cancel-by-cloid `{"asset":…, "cloid":…}` — note the FULL key names here (unlike
/// [`CancelWire`]'s `a`/`o`).
#[derive(Serialize, Clone, Debug)]
pub struct CancelCloidWire {
    pub asset: u32,
    /// Client order id (`0x` + 32 hex).
    pub cloid: String,
}

/// The `cancelByCloid` action body: `{"cancels":[{"asset":…,"cloid":…}]}`.
#[derive(Serialize, Clone, Debug)]
pub struct CancelByCloidAction {
    pub cancels: Vec<CancelCloidWire>,
}

/// A single modify `{"oid":…, "order":{…}}`. Reused verbatim as the `modify` action body (via the
/// [`Action::Modify`] newtype) and as each element of [`BatchModifyAction::modifies`].
#[derive(Serialize, Clone, Debug)]
pub struct ModifyWire {
    /// The exchange oid of the order being replaced.
    pub oid: u64,
    /// The replacement order.
    pub order: OrderWire,
}

/// The `batchModify` action body: `{"modifies":[{"oid":…,"order":{…}}]}`.
#[derive(Serialize, Clone, Debug)]
pub struct BatchModifyAction {
    pub modifies: Vec<ModifyWire>,
}

#[cfg(test)]
mod tests {
    //! The two Builder Codes correctness properties (task 7): the wire shape when a builder IS
    //! configured, and — the money-critical one — that the ABSENT path reproduces today's signed
    //! bytes exactly (see [`super::hash::action_hash`]'s test twin below for the hash-level proof;
    //! this module proves the msgpack-adjacent JSON shape).
    use super::*;

    fn one_order() -> OrderWire {
        OrderWire {
            asset: 1,
            is_buy: true,
            limit_px: "2000.0".to_string(),
            sz: "3.5".to_string(),
            reduce_only: false,
            order_type: OrderKind::Limit(LimitParams { tif: "Gtc".to_string() }),
            cloid: None,
        }
    }

    #[test]
    fn order_wire_carries_builder_when_configured_and_omits_when_not() {
        let with = Action::Order(OrderAction {
            orders: vec![one_order()],
            grouping: "na".to_string(),
            builder: Some(HlBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 }),
        });
        let j = serde_json::to_value(&with).unwrap();
        assert_eq!(j["builder"]["b"], "0x0c8d");
        assert_eq!(j["builder"]["f"], 0);

        let without = Action::Order(OrderAction {
            orders: vec![one_order()],
            grouping: "na".to_string(),
            builder: None,
        });
        let j2 = serde_json::to_value(&without).unwrap();
        assert!(j2.get("builder").is_none() || j2["builder"].is_null());
    }
}
