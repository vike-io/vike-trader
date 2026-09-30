//! Polymarket `ReconClient` — the venue-facing report seam (`vike_exec::recon::ReconClient`) for a
//! PREDICTION MARKET. Polymarket is NOT a CEX, so the fit was investigated before building; the
//! finding is that all four report kinds map meaningfully, and the mapping is documented here so a
//! future reader understands what recon does — and, as importantly, does NOT — cover for an on-chain
//! settled venue.
//!
//! ## The four report kinds, and how a prediction market maps onto them
//! - **Positions** (`fetch_position_status_reports`): the data-api `GET /positions?user=<funder>`
//!   read (the SAME endpoint [`crate::exec_plane::settlement::positions`] uses for CTF auto-redeem) — a per-outcome-token
//!   holding list. A Polymarket "position" is a balance of an ERC-1155 outcome `token_id` (you can
//!   only be LONG a token; to be "short YES" you hold the NO token, a DIFFERENT `token_id`), so every
//!   row is `qty >= 0`, keyed by `token_id`, `position_side = Both` — the spot/one-way convention,
//!   NOT a signed net perp position. `avg_px` comes from the row's `avgPrice`. This is the CORE
//!   reconcilable state and needs no auth (the funder address is the key). Account-wide (one call
//!   returns every holding), so — unlike the per-symbol IBKR/ctrader/alpaca clients — there is no
//!   conId filter and no synthesized flat row: `recon::diff::diff` iterates the venue rows and flags
//!   drift / external-only, exactly as it does for the account-wide crypto-spot venues.
//! - **Balance** (`fetch_balance`): the CLOB `GET /balance-allowance?asset_type=COLLATERAL` read
//!   (L2-signed) — the account's USDC collateral, the venue "cash" `diff_balance` reconciles against.
//!   Base units are 6-decimal (USDC/CTF), so the wire value is divided by 1e6.
//! - **Fills** (`fetch_fill_reports`): the CLOB `GET /data/trades` history (L2-signed), decoded with
//!   the SAME registry re-keying + composite `trade_id` the live user-WS path
//!   ([`crate::exec_plane::user_ws::decode_user`]) and the A3 resync ([`crate::exec_plane::history`]) use — a trade lists our
//!   order as `taker_order_id` (top-level size/price) or inside `maker_orders[]` (that entry's
//!   matched_amount/price); each is re-keyed CLOB→coid via the shared [`PolymarketRegistry`] and
//!   stamped `trade_id = "{id}:{order_id}"`. That composite is load-bearing: it is the exact string
//!   the live path records in `seen_trade_ids`, so a fill both paths saw dedups (no false
//!   `MissingFill`) while a genuinely-missed fill still surfaces. Only the fillable statuses
//!   (`MATCHED`/`MINED`/`CONFIRMED`) count — the same set the live decode folds on.
//! - **Orders** (`fetch_order_status_reports`): the CLOB `GET /data/orders` active-order list
//!   (L2-signed), re-keyed CLOB→coid via the registry so `recon::diff::diff` can match each against
//!   local by `client_order_id` (Polymarket assigns the order `id`; there is NO client-id echo — the
//!   registry is the only bridge back to the coid, exactly as on the live path). An order the
//!   registry does not know (externally placed, or a cold registry after a restart) carries
//!   `client_order_id = None` → an ordinary `UnknownOrder`, the same fail-soft every venue uses. The
//!   active-order status is derived from fill progress (see [`normalize_order_status`]).
//!
//! ## The on-chain-settlement boundary — and the watcher that now covers it
//! Polymarket settlement is a two-step on-chain flow — a market RESOLVES, then winning tokens are
//! REDEEMED for USDC — and redemption produces NO CLOB trade. So a redeem moves a position → cash
//! WITHOUT a `/data/trades` fill: recon surfaces the cash side (a `BalanceDrift` until local folds
//! it) but the position drop is an account-wide blind spot (the diff has no orphan-local-position
//! check, the same as every account-wide venue). Driving settlement is the job of the crate's
//! separate [`crate::exec_plane::settlement::resolve`] / [`crate::exec_plane::settlement::auto_redeem`] pollers, which fold the settlement fill that
//! then flattens the local position too; this seam is the periodic divergence net, complementary to
//! them, not a replacement.
//!
//! **[`crate::exec_plane::settlement::chain`] closes that boundary from the other side**, opt-in and default-off. Given a
//! [`ChainOracle`](crate::exec_plane::settlement::chain::ChainOracle) — attached with
//! [`PolymarketReconClient::with_chain_oracle`] — [`fetch_fill_reports`](ReconClient::fetch_fill_reports)
//! APPENDS one settlement `FillReport` per on-chain redemption of this account's own tokens, read
//! straight from Polygon (`PayoutRedemption` joined to the funder's ERC-1155 outflow). Each carries
//! the SAME `trade_id` [`crate::exec_plane::settlement::resolve::settlement_trade_id`] stamps, which is the whole point:
//!
//! - if the resolve poller already folded that settlement, the id is in `seen_trade_ids` and the
//!   diff DEDUPS it — no divergence at all;
//! - if it did not, the movement surfaces as an ordinary `MissingFill` carrying the real payout, so
//!   it is **explained** (a fill an operator can fold, or `hybrid` can fold automatically) instead
//!   of appearing as unexplained cash with a vanished position.
//!
//! Without an oracle the method is byte-identical to the CLOB-only fetch it always was.
//!
//! ## ⚠ ROLLOUT — quarantine-first, and DOUBLE-gated
//! This client IS now wired into `vike_mount::make_engine`'s `("polymarket", _)` arm, behind a
//! venue-local opt-in ON TOP of the workspace master gate — BOTH, and the arm reads both
//! (`vike_mount`'s `poly_recon_wanted`). ⚠ **What S2 changed is which of the two an operator still
//! types.** The master gate is now ON by default for a mount that arms a live venue account
//! (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`), so on a live box
//! `flags.poly_reconcile` ALONE now starts authenticated Polygon-MAINNET reconcile passes against this
//! real-money venue — where before it needed `VIKE_RECONCILE=1` beside it and otherwise built a
//! client nothing used. The venue gate survives the default deliberately and is still an act no
//! other venue needs; `VIKE_RECONCILE_OFF=1` refuses the outer one. See
//! [`poly_reconcile_enabled`] for the full reasoning. The short version, and it is the thing to read
//! before enabling anything:
//!
//! 1. **Exec may not be mounted** (`flags.poly_exec` is an independent gate), in which case the local
//!    side of the diff is a PAPER engine, not this venue's twin. Every real holding then diffs as
//!    `PositionOnlyExternal` and every resting CLOB order as `UnknownOrder` — both no-local-origin,
//!    so `hybrid` quarantines them. A paper order diffs as `OrphanLocalOrder`, which folds
//!    **nothing** under every policy and, under `hybrid`/`quarantine`, surfaces ONE aggregated,
//!    dedup-keyed, event-free operator alert for the whole paper book — expect that alert on a
//!    recon-without-exec mount; it is the configuration reporting itself, not a fault.
//!
//!    ⚠ CORRECTED 2026-08-06. This paragraph used to claim a paper `OrphanLocalOrder`
//!    "auto-cancels every pass" under `hybrid` and called that the one genuinely destructive
//!    outcome; the same sentence appears in five other places and drove the quarantine-first
//!    rollout rule for every newly-wired venue. **It was false.**
//!    `vike_exec::recon::resolve`'s `events_for` has no `OrphanLocalOrder` arm, so the kind lands
//!    in its `_ => (Vec::new(), false)` catch-all: zero events. `resolve` constructs no
//!    `Event::OrderCanceled` anywhere, for any kind, under any policy. The only code that
//!    terminalizes an order the venue stopped reporting is `ExecutionEngine::apply_snapshot`'s
//!    reap — which synthesizes a **local** cancel through the FSM and never calls the venue — and
//!    the live reconcile path cannot reach it, because `vike_core`'s
//!    `CoreThread::reconcile_reports` documents that it deliberately avoids
//!    `Command::ApplySnapshot` for exactly that reason. Pinned by
//!    `crates/vike-exec/tests/recon/recon_policy_pin.rs`. (⚠ AMENDED: the correction also said `hybrid`
//!    mapped the kind to `Synthesize` so "not even an alert" was raised. That WAS true and was the
//!    second defect — detection with no outcome. The kind is now classified no-local-origin, so
//!    held policies surface it; it still folds nothing, so the retraction above stands whole.)
//! 2. **The redeem blind spot** — a resolved market's redeem moves position → cash with NO
//!    `/data/trades` fill, so a hybrid policy that auto-folded position divergences would be folding
//!    a settlement it cannot see the other half of. `quarantine` puts an operator in front of
//!    exactly that decision. This one is now ADDRESSABLE: wire [`crate::exec_plane::settlement::chain`]'s oracle
//!    (`POLY_CHAIN_WATCH=1`) and the redeem arrives as an explained `MissingFill` instead.
//!
//! **The real exposure of a recon-without-exec mount is `PositionDrift`, not `OrphanLocalOrder`.**
//! It is the one kind `hybrid` genuinely auto-applies (`vike_tradehub::reconcile_config`'s
//! `auto_applied_kinds` is the machine-checked list), and against a paper engine that traded the
//! same token it folds the LIVE account's position into the PAPER engine's books at the venue's avg
//! price, booking realized PnL the paper run never earned. That is local-state corruption, not a
//! venue-side action — no order is placed, moved or cancelled — but it feeds `RiskGate` and every
//! PnL number thereafter, and it is the same blind spot blocker (2) describes. So the supported
//! configuration for a recon-only mount is `flags.poly_reconcile` on, with the master gate on and
//! `VIKE_RECONCILE_POLICY` left alone — since S2 both live roots fold `quarantine` in as the
//! default policy, and a live mount supplies the master gate itself, so the configuration that
//! used to need three explicit settings now needs one. (Spelling `VIKE_RECONCILE=1` beside it is
//! still correct and now redundant; it is a FORCE-ON, not the switch.) With `flags.poly_reconcile`
//! off — the default — `make_engine` builds nothing and makes no network call, and the same is true
//! with it on for a mount whose reconcile gate answered no.
//!
//! ## Wire shapes are documented, the PARSERS are the proven deliverable
//! Every mapping is a PURE free function (`parse_*`) over the raw JSON, fixture-tested offline in
//! `tests/offline/polymarket_reconcile_parse.rs` (synthetic bodies — no network). The trade/order field
//! names are the ones [`crate::exec_plane::user_ws`] already decodes live; the `/data/orders`, `/positions` and
//! `/balance-allowance` shapes are the documented Polymarket CLOB / data-api shapes. Polymarket is
//! US-geo-blocked, so the `#[ignore]`d `tests/polymarket_reconcile_smoke.rs` live-verifies the
//! fetch+parse read-only through the Dublin proxy (self-skips without creds). The status vocabulary
//! is the ONE `OrderStatus::parse` FSM vocabulary the exec path already speaks.

use std::sync::Arc;

use serde_json::Value;
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::config::{CLOB_BASE, DATA_API_BASE, PolymarketCreds, first_token};
use crate::egress::get_json;
use crate::exec_plane::exec::{get_orders, get_signed, get_trades};
use crate::exec_plane::order::SignatureType;
use crate::exec_plane::registry::PolymarketRegistry;
use crate::exec_plane::settlement::chain::{ChainOracle, ChainSettlement};

/// The venue key stamped on every report row.
pub const VENUE: &str = "polymarket";

/// USDC and every Polymarket CTF outcome token use 6 decimals — the balance-allowance wire value is
/// in base units, so it is divided by this to reach whole USDC.
const USDC_DECIMALS: f64 = 1_000_000.0;

/// Default recon lookback (row count) for the `/data/orders` + `/data/trades` reads. `since` is a
/// no-op: the CLOB history endpoints page by cursor/limit, not an epoch cutoff (documented, not a
/// silent omission — see the `fetch_*` docs).
const DEFAULT_FETCH_LIMIT: u32 = 500;

// --- pure helpers -------------------------------------------------------------------------------

/// A numeric field that may arrive as a JSON number OR a decimal string (Polymarket quotes most
/// sizes/prices) — absent/`null`/unparseable folds to `0.0` rather than erroring the row.
fn num(v: &Value, key: &str) -> f64 {
    v.get(key).map(num_val).unwrap_or(0.0)
}

/// A single JSON value → `f64`, tolerating both the number and the quoted-string encodings.
fn num_val(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0.0)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// `"SELL"` → `-1`, everything else (`"BUY"`/empty) → `+1` — the inverse of
/// [`crate::exec_plane::client`]'s `if req.side >= 0 { Side::Buy } else { Side::Sell }`.
fn side_sign(s: &str) -> i32 {
    if s.eq_ignore_ascii_case("SELL") { -1 } else { 1 }
}

/// The array of rows from a Polymarket list response — a bare `[..]`, or wrapped under a `data` key
/// (the paginated `{data:[..], next_cursor, ..}` shape some CLOB list endpoints use). `None` when the
/// body is neither.
fn rows(v: &Value) -> Option<&Vec<Value>> {
    v.as_array().or_else(|| v.get("data").and_then(|d| d.as_array()))
}

/// A confirmed-good trade status → a fill. Mirrors `user_ws::is_fillable_status` (the SAME set the
/// live decode folds on, so the composite `trade_id` dedups against `seen_trade_ids`);
/// `RETRYING`/`FAILED` are skipped.
fn is_fillable_status(status: &str) -> bool {
    matches!(status, "MATCHED" | "MINED" | "CONFIRMED")
}

/// A trade row's timestamp (epoch) → `i64`, tolerating the field-name variants the CLOB uses across
/// the WS frame (`timestamp`) and the `/data/trades` REST rows (`match_time`); `last_update` is a
/// final fallback. `0` when none is present (ts is informational — dedup keys on `trade_id`).
fn trade_ts(t: &Value) -> i64 {
    for k in ["match_time", "timestamp", "last_update"] {
        let x = num(t, k);
        if x != 0.0 {
            return x as i64;
        }
    }
    0
}

/// Normalize a Polymarket CLOB active order to the `OrderStatus::parse` FSM vocabulary.
/// `/data/orders` lists ACTIVE (resting) orders, so status is derived from fill progress
/// (`size_matched` vs `original_size`) — the same progress split IBKR/ctrader use for a venue whose
/// resting status carries no distinct partial value: fully matched → `FILLED`, partial →
/// `PARTIALLY_FILLED`, untouched → `ACCEPTED`. A raw status NAMING a cancel (defensive — the active
/// list should not carry one) → `CANCELED`.
pub fn normalize_order_status(raw: &str, matched: f64, original: f64) -> String {
    if raw.to_ascii_uppercase().contains("CANCEL") {
        return "CANCELED".to_string();
    }
    if original > 0.0 && matched >= original {
        "FILLED"
    } else if matched > 0.0 {
        "PARTIALLY_FILLED"
    } else {
        "ACCEPTED"
    }
    .to_string()
}

// --- pure parsers (fixture-tested over the raw JSON bodies) --------------------------------------

/// `GET /data/orders` (a bare array, or `{data:[..]}`) → `OrderStatusReport`. Each row is re-keyed
/// CLOB `id` → coid via `lookup` (the shared registry's `lookup_clob`); an order the registry does
/// not know maps to `client_order_id: None` (an externally-placed / cold-registry order →
/// `UnknownOrder`, never a panic). `asset_id` → symbol (the outcome `token_id`); `original_size` →
/// qty; `size_matched` → filled_qty; `avg_px` is `0.0` (the active-orders row carries the LIMIT
/// price, not an average FILL price, and the diff does not read order `avg_px`); status via
/// [`normalize_order_status`]; `created_at` → ts.
pub fn parse_order_reports(
    v: &Value,
    lookup: impl Fn(&str) -> Option<(String, i32)>,
) -> Result<Vec<OrderStatusReport>, String> {
    let rows = rows(v).ok_or("expected an orders array")?;
    Ok(rows
        .iter()
        .map(|o| {
            let clob_id = str_field(o, "id");
            let original = num(o, "original_size");
            let matched = num(o, "size_matched");
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: str_field(o, "asset_id"),
                client_order_id: lookup(&clob_id).map(|(coid, _)| coid),
                venue_order_id: clob_id.into(),
                side: side_sign(&str_field(o, "side")),
                order_type: str_field(o, "order_type").to_ascii_lowercase(),
                qty: original,
                filled_qty: matched,
                avg_px: 0.0,
                status: normalize_order_status(&str_field(o, "status"), matched, original),
                ts: num(o, "created_at") as i64,
            }
        })
        .collect())
}

/// `GET /data/trades` (a bare array, or `{data:[..]}`) → `FillReport`, ONE per order of ours the
/// trade matched. A trade lists our order as `taker_order_id` (top-level size/price →
/// `LiquiditySide::Taker`) or inside `maker_orders[]` (that entry's `matched_amount`/`price` →
/// `LiquiditySide::Maker`); each is re-keyed CLOB→coid via `lookup`, and rows for NO order of ours
/// emit nothing (not our fill to fold — the SAME contract as `user_ws::decode_user` /
/// `history::map_polymarket_history`). The composite `trade_id = "{id}:{order_id}"` and the
/// REGISTERED side (not the trade's top-level taker side) match the live path exactly, so a fill
/// both paths saw dedups. Only fillable statuses count. `commission`/`commission_asset` are
/// `0.0`/`""` (Polymarket charges no per-fill commission on this wire — as the live decode also
/// records).
pub fn parse_fill_reports(
    v: &Value,
    lookup: impl Fn(&str) -> Option<(String, i32)>,
) -> Result<Vec<FillReport>, String> {
    let rows = rows(v).ok_or("expected a trades array")?;
    let mut out = Vec::new();
    for t in rows {
        if !is_fillable_status(&str_field(t, "status")) {
            continue;
        }
        let trade_id = str_field(t, "id");
        // The WIRE half of the composite must be real. A trade with no `id` would render
        // `":{order_id}"` — an id that is per-ORDER rather than per-FILL, so every later fill of the
        // same order renders the SAME string and the reports collapse into one, silently losing
        // real economics. Worse on this side than on the live one: an id that cannot match
        // `seen_trade_ids` FABRICATES a `MissingFill`, and `MissingFill` is one of the two kinds the
        // `hybrid` policy AUTO-APPLIES — so the invented divergence books the fill again with no
        // operator in front of it. Skip the whole row (both legs): the next pass re-reads the same
        // window, so a skipped row costs at most one pass of visibility.
        if trade_id.is_empty() {
            tracing::warn!(
                venue = VENUE,
                "reconcile /data/trades row carries no `id` — skipping it; a composite trade_id \
                 with an empty wire half cannot match `seen_trade_ids` and would manufacture a \
                 MissingFill that `hybrid` auto-applies (double-booking the fill)"
            );
            continue;
        }
        let asset_id = str_field(t, "asset_id");
        let ts = trade_ts(t);
        // taker side: our order == taker_order_id → top-level size/price.
        let taker = str_field(t, "taker_order_id");
        if let (Some((coid, side)), Some(tid)) =
            (lookup(&taker), composite_trade_id(&trade_id, &taker))
        {
            out.push(fill_report(
                tid,
                &taker,
                &asset_id,
                coid,
                side,
                num(t, "size"),
                num(t, "price"),
                LiquiditySide::Taker,
                ts,
            ));
        }
        // maker side: each maker_orders[].order_id that is ours → that entry's matched_amount/price.
        for mo in t.get("maker_orders").and_then(|m| m.as_array()).into_iter().flatten() {
            let oid = str_field(mo, "order_id");
            if let (Some((coid, side)), Some(tid)) =
                (lookup(&oid), composite_trade_id(&trade_id, &oid))
            {
                out.push(fill_report(
                    tid,
                    &oid,
                    &asset_id,
                    coid,
                    side,
                    num(mo, "matched_amount"),
                    num(mo, "price"),
                    LiquiditySide::Maker,
                    ts,
                ));
            }
        }
    }
    Ok(out)
}

/// The composite id BOTH this parser and the live `crate::exec_plane::user_ws` decode stamp:
/// `"{trade_id}:{order_clob_id}"`. That equality is the dedup contract — a fill both paths saw must
/// render one string — so the shape is fixed.
///
/// [`None`] only when the composite would be empty, which its callers have already ruled out by
/// refusing a row whose wire `id` is empty. It stays fallible rather than becoming an `expect`
/// because a venue frame must never reach a panicking constructor: a malformed frame costs a
/// dropped row, never the process.
fn composite_trade_id(trade_id: &str, order_clob_id: &str) -> Option<TradeId> {
    TradeId::new(format!("{trade_id}:{order_clob_id}")).ok()
}

#[allow(clippy::too_many_arguments)]
fn fill_report(
    trade_id: TradeId,
    order_clob_id: &str,
    asset_id: &str,
    coid: String,
    side: i32,
    qty: f64,
    px: f64,
    liquidity_side: LiquiditySide,
    ts: i64,
) -> FillReport {
    FillReport {
        venue: VENUE.to_string(),
        symbol: asset_id.to_string(),
        trade_id,
        venue_order_id: order_clob_id.into(),
        client_order_id: Some(coid),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new(),
        liquidity_side,
        ts,
    }
}

/// data-api `GET /positions?user=<funder>` (a bare array, or `{data:[..]}`) → `PositionStatusReport`.
/// `asset` (the ERC-1155 outcome `token_id`) → symbol; `size` → qty (always `>= 0` — a Polymarket
/// holding is one-way); `avgPrice` → avg_px; `position_side = Both` (spot/one-way, matching the
/// `"BOTH"` the live fills fold under). A row with an empty `asset` is skipped (tolerant, like
/// `positions::parse_positions`). Zero-size rows are KEPT when present — they let the diff detect a
/// stale local position the venue now reports flat.
pub fn parse_position_reports(v: &Value) -> Result<Vec<PositionStatusReport>, String> {
    let rows = rows(v).ok_or("expected a positions array")?;
    Ok(rows
        .iter()
        .filter_map(|p| {
            let asset = str_field(p, "asset");
            if asset.is_empty() {
                return None;
            }
            Some(PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: asset,
                position_side: PositionSide::Both,
                qty: num(p, "size"),
                avg_px: num(p, "avgPrice"),
                ts: 0,
                margin_mode: MarginMode::default(),
                isolated_margin: None,
                delta: None,
            })
        })
        .collect())
}

/// One observed on-chain settlement → the `FillReport` that EXPLAINS it to reconcile.
///
/// The `trade_id` is [`crate::exec_plane::settlement::resolve::settlement_trade_id`]'s exact string, so a settlement the
/// resolve poller already folded dedups against `local.seen_trade_ids` and produces no divergence at
/// all; one it has not folded surfaces as a `MissingFill` an operator (or `hybrid`) can apply. The
/// fill CLOSES a long — `side = -1`, `last_qty` = the redeemed shares, `last_px` = the collateral
/// per token the chain actually paid — mirroring `resolve::settlement_fill`'s construction so both
/// paths book the identical realised PnL. `venue_order_id`/`client_order_id` carry the
/// `resolution:<conditionId>` tag (a settlement has no order behind it), and `liquidity_side` is
/// `Unknown`: a settlement is neither maker nor taker.
pub fn settlement_fill_report(s: &ChainSettlement) -> FillReport {
    FillReport {
        venue: VENUE.to_string(),
        symbol: s.token_id.clone(),
        trade_id: crate::exec_plane::settlement::resolve::settlement_trade_id(
            &s.condition_id,
            &s.token_id,
        ),
        venue_order_id: format!("resolution:{}", s.condition_id).into(),
        client_order_id: None,
        side: -1,
        last_qty: s.qty,
        last_px: s.price,
        commission: 0.0,
        commission_asset: String::new(),
        liquidity_side: LiquiditySide::Unknown,
        ts: s.ts_ms,
    }
}

/// CLOB `GET /balance-allowance?asset_type=COLLATERAL` → the `balance` (USDC collateral) in whole
/// USDC (the wire value is 6-decimal base units, so it is divided by 1e6). `None` when the field is
/// absent (fail-soft); a non-object body is a hard `Err`, never a panic.
pub fn parse_balance(v: &Value) -> Result<Option<f64>, String> {
    if !v.is_object() {
        return Err("expected a balance-allowance object".to_string());
    }
    Ok(v.get("balance").map(num_val).map(|b| b / USDC_DECIMALS))
}

// --- the client ---------------------------------------------------------------------------------

/// One account-wide Polymarket reconcile client. Holds its OWN [`PolymarketCreds`] (the L2 trio for
/// the signed order/fill/balance reads), the funder address (the data-api positions key), the
/// account's [`SignatureType`] (the balance-allowance cache is keyed by it), and a clone of the
/// shared [`PolymarketRegistry`] — the SAME map the exec thread writes and the user-WS pump reads, so
/// order/fill reports re-key CLOB→coid identically to the live path. `ReconClient`'s methods take
/// `&self`; the registry is `Arc`-backed and the `ureq` agent is `Send + Sync`, so no interior
/// mutability is needed.
pub struct PolymarketReconClient {
    creds: PolymarketCreds,
    clob_base: String,
    data_api: String,
    funder: String,
    signature_type: SignatureType,
    registry: PolymarketRegistry,
    limit: u32,
    /// OPTIONAL on-chain settlement oracle ([`crate::exec_plane::settlement::chain`]) — `None` (the default) leaves every
    /// fetch below byte-identical to the CLOB-only client.
    chain: Option<Arc<ChainOracle>>,
}

impl PolymarketReconClient {
    /// Build a reconcile client against the production CLOB + data-api hosts. `funder` is the
    /// data-api positions key (the deposit-wallet / proxy / EOA address whose holdings are read);
    /// `signature_type` matches the exec wallet (the balance-allowance cache is per-signature-type);
    /// pass the SAME `registry` clone the [`crate::exec_plane::client::PolymarketExecutionClient`] and user-WS
    /// pump share so orders/fills re-key to the live coids.
    pub fn new(
        creds: PolymarketCreds,
        funder: String,
        signature_type: SignatureType,
        registry: PolymarketRegistry,
    ) -> Self {
        PolymarketReconClient {
            creds,
            clob_base: CLOB_BASE.to_string(),
            data_api: DATA_API_BASE.to_string(),
            funder,
            signature_type,
            registry,
            limit: DEFAULT_FETCH_LIMIT,
            chain: None,
        }
    }

    /// Attach the on-chain settlement oracle: from here on `fetch_fill_reports` also reports the
    /// redemptions it has observed (see the module doc's on-chain-settlement section). Opt-in — the
    /// caller only builds an oracle when [`crate::exec_plane::settlement::chain::chain_watch_enabled`].
    pub fn with_chain_oracle(mut self, oracle: Arc<ChainOracle>) -> Self {
        self.chain = Some(oracle);
        self
    }

    /// The chain-derived half of [`fetch_fill_reports`](ReconClient::fetch_fill_reports): the
    /// settlements observed at or after `since`, as `FillReport`s. Empty without an oracle.
    fn chain_settlement_fills(&self, since: i64) -> Vec<FillReport> {
        let Some(oracle) = self.chain.as_ref() else { return Vec::new() };
        oracle.settlements_since(since).iter().map(settlement_fill_report).collect()
    }
}

impl ReconClient for PolymarketReconClient {
    /// `_since` is a no-op: `/data/orders` returns the CURRENTLY active set (cursor/limit paging, no
    /// epoch cutoff) — a closed order that fell out of it is what the fill/position reports reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let v = get_orders(&self.clob_base, &self.creds, self.limit)?;
        parse_order_reports(&v, |id| self.registry.lookup_clob(id))
    }

    /// `since` is a no-op for the CLOB half: `/data/trades` returns a recent-executions window
    /// bounded by `limit`, not by an epoch cutoff (the reconcile lookback is that window). Trades
    /// for no order of ours emit nothing.
    ///
    /// It IS honoured by the chain half: when an oracle is attached, the on-chain settlements it has
    /// observed at or after `since` are APPENDED as settlement fills — the redeem-shaped movement no
    /// CLOB endpoint reports (module doc). Without an oracle this is the CLOB fetch, unchanged.
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let v = get_trades(&self.clob_base, &self.creds, self.limit)?;
        let mut out = parse_fill_reports(&v, |id| self.registry.lookup_clob(id))?;
        out.extend(self.chain_settlement_fills(since));
        Ok(out)
    }

    /// The funder's account-wide token holdings (data-api `/positions`, unauthenticated — the funder
    /// address is the key). Account-wide, so an absent token is simply not held: no flat-row synthesis
    /// (the account-wide diff has no orphan-local-position check — see the module doc).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let v = get_json(&self.data_api, "/positions", &format!("user={}", self.funder))?;
        parse_position_reports(&v)
    }

    /// The account's USDC collateral (`/balance-allowance?asset_type=COLLATERAL`, keyed by the
    /// account's signature type) — the venue cash `diff_balance` reconciles against.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let query = format!("asset_type=COLLATERAL&signature_type={}", self.signature_type.code());
        let v = get_signed(&self.clob_base, "/balance-allowance", &query, &self.creds)?;
        parse_balance(&v)
    }
}

// --- the factory --------------------------------------------------------------------------------

/// The venue → `ReconClient` factory (the ReconFactory seam) for Polymarket. `None` when the L2 trio
/// is absent (`api_key`/`secret` empty) — the order/fill/balance reads are L2-signed, so without it
/// there is nothing to reconcile with; reconcile stays unwired and exec is unaffected (the same
/// absent-creds-is-the-live-gate discipline every bridge follows). Construction is otherwise
/// infallible (no network), so a fully-credentialed call always returns `Some`.
///
/// **Wired into `vike_mount::make_engine`'s `("polymarket", _)` arm** — through the
/// [`recon_client_from_vars`] convenience below, which does the L1→L2 derivation this raw factory
/// takes as already-done. Call THIS one when you already hold derived L2 creds and the SAME
/// registry the exec client writes (the live-mount path); call [`recon_client_from_vars`] from a
/// composition root that only has the workspace `.env` map.
pub fn recon_client(
    creds: PolymarketCreds,
    funder: String,
    signature_type: SignatureType,
    registry: PolymarketRegistry,
) -> Option<Box<dyn ReconClient>> {
    if creds.api_key.is_empty() || creds.secret.is_empty() {
        tracing::warn!(target: "vike_polymarket::recon", "Polymarket recon client unwired: no L2 creds");
        return None;
    }
    Some(Box::new(PolymarketReconClient::new(creds, funder, signature_type, registry)))
}

/// The name the daemon folds `flags.poly_reconcile` into the credential map under (see
/// [`poly_reconcile_enabled`]).
pub const POLY_RECONCILE_ENV: &str = "POLY_RECONCILE";

/// The SECOND gate on Polymarket reconciliation, on top of the workspace-wide master gate: the
/// EXACT string `"1"` in the caller's map, default OFF. The arm
/// requires BOTH (`vike_mount`'s `poly_recon_wanted`) — this one decides whether the VENUE wants a
/// read client, the master gate whether this process mounts the driver that would use one.
///
/// ⚠ **The master gate is no longer `VIKE_RECONCILE=1`, and that changes what setting THIS flag
/// alone does.** Since S2 (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`)
/// reconciliation is ON by default for any mount that arms a live venue account, so on such a box
/// `flags.poly_reconcile` by itself now starts authenticated Polygon-MAINNET passes against a
/// real-money venue with no testnet — where before it needed a second variable and otherwise built
/// a client the root dropped. Nothing about the reasoning below changes; what changes is that the
/// second act it assumed an operator would perform is now performed by the box. The refusals are
/// `flags.poly_reconcile` off (this venue only) and `VIKE_RECONCILE_OFF=1` (every venue).
///
/// **Why Polymarket needs its own gate when no other venue does.** Every other reconciled venue
/// mounts a LIVE `ExecutionClient` in the same `make_engine` arm that builds its `ReconClient`, so
/// the local side of the diff is the exact twin of the venue side. Polymarket's exec client CAN now
/// be mounted there ([`crate::exec_plane::mount::live_mount_from_vars`], behind its own `flags.poly_exec`) — but the
/// two gates are INDEPENDENT, so this one alone still leaves the local side as whatever engine the
/// app mounted for the venue, i.e. a PAPER one. Reconciling live account state against a paper
/// engine is wrong in a specific, actionable way:
///
/// - every real holding diffs as `PositionOnlyExternal`, every resting CLOB order as
///   `UnknownOrder` — under `hybrid` both are no-local-origin, so they QUARANTINE (safe: alerts an
///   operator reviews, nothing folds);
/// - a PAPER order will never appear in `/data/orders`, so it diffs as `OrphanLocalOrder` — which
///   FOLDS nothing under every policy and, under `hybrid`/`quarantine`, raises ONE aggregated,
///   event-free operator alert naming the paper book. Informative, not destructive: nothing an
///   operator confirm could apply, and nothing that reaches the venue.
///
///   ⚠ CORRECTED 2026-08-06: this bullet used to read "**auto-cancels every pass** … the one
///   genuinely destructive outcome, and it is why this gate exists and why the rollout is
///   quarantine-first". It was wrong — `resolve` emits no events for the kind and no
///   `Event::OrderCanceled` for any kind. See this module's `## ⚠ ROLLOUT` doc for the full trace
///   and `crates/vike-exec/tests/recon/recon_policy_pin.rs` for the pin.
/// - the kind that DOES auto-apply under `hybrid` is **`PositionDrift`**, and against a paper
///   engine it folds the live account's position into the paper engine's books at the venue's avg
///   price. Local-state corruption rather than a venue-side action, but it is the real reason this
///   configuration wants `quarantine`.
///
/// So the policy is decided by which of the two configurations is running:
/// - `flags.poly_reconcile` **without** `flags.poly_exec` — the paper-vs-live diff above.
///   **`VIKE_RECONCILE_POLICY=quarantine` is required**, on the `PositionDrift` ground. Since S2
///   that is what an unset variable resolves to at both live roots
///   (`vike_tradehub::reconcile_config::quarantine_first_default`), so this requirement is now satisfied
///   by DEFAULT rather than by an operator remembering it — and a mount that NAMES `hybrid` is the
///   configuration to refuse.
/// - `flags.poly_reconcile` **with** `flags.poly_exec` — `make_engine` builds both halves from ONE
///   `live_mount_from_vars` call over one shared `PolymarketRegistry`, so a local order really does
///   exist at the venue and re-keys to its coid, and `PositionDrift` converges two views of the
///   SAME account rather than importing one into the other — so `hybrid` is sound. (One residual,
///   unchanged by exec mounting: a resolved market moves position → cash with NO `/data/trades`
///   fill, so a redeemed holding still shows up as a `PositionDrift` — which `hybrid`
///   auto-applies. `POLY_CHAIN_WATCH=1` is what turns that into an explained `MissingFill`.)
///
/// Unset (the default) ⇒ `make_engine` builds no client and makes NO network call — byte-identical
/// to before this was wired.
///
/// Map-only since decision 0095: `flags.poly_reconcile` is a settings row the daemon folds into
/// this map, and a `POLY_RECONCILE` variable refuses startup (`vike_config::REMOVED_ENV`).
pub fn poly_reconcile_enabled(vars: &std::collections::HashMap<String, String>) -> bool {
    vars.get(POLY_RECONCILE_ENV).map(|v| first_token(v)) == Some("1")
}

/// `POLY_SIGNATURE_TYPE` → [`SignatureType`], defaulting to `Poly1271` (the deposit-wallet account
/// type this repo's live account uses). Mirrors the same parse in
/// [`crate::exec_plane::client`]'s live-mount helper and in the reconcile smoke, kept here so the mount path and
/// the smoke agree by construction.
pub fn signature_type_from_vars(vars: &std::collections::HashMap<String, String>) -> SignatureType {
    signature_type_for_account(vars, &vike_model::account_keys::AccountLabel::Default)
}

/// [`signature_type_from_vars`] for ONE NAMED ACCOUNT — `POLY_SIGNATURE_TYPE__{LABEL}`.
///
/// ⚠ **Per-ACCOUNT, not per-deployment, and the reason is that it is a property of the WALLET.** A
/// bare-key EOA and a deposit wallet sign differently, and two Polymarket accounts on one box may
/// be one of each — a labelled account inheriting the default account's type would sign its orders
/// in a shape the venue rejects (or, worse, name the wrong maker).
///
/// ⚠ **No fallback to the unlabelled key**: an unset `POLY_SIGNATURE_TYPE__ALT` reads the same
/// `Poly1271` DEFAULT an unset `POLY_SIGNATURE_TYPE` always has — the type's own default, never the
/// other account's value. [`AccountLabel::Default`](vike_model::account_keys::AccountLabel::Default)
/// is [`signature_type_from_vars`], reached through it.
pub fn signature_type_for_account(
    vars: &std::collections::HashMap<String, String>,
    label: &vike_model::account_keys::AccountLabel,
) -> SignatureType {
    let key = vike_model::account_keys::account_key("POLY_SIGNATURE_TYPE", label);
    match vars.get(&key).map(|s| first_token(s)) {
        Some("0") => SignatureType::Eoa,
        Some("1") => SignatureType::PolyProxy,
        Some("2") => SignatureType::PolyGnosisSafe,
        _ => SignatureType::Poly1271,
    }
}

/// Build the reconcile client straight from the workspace `.env` map — the seam
/// `vike_mount::make_engine`'s `("polymarket", _)` arm calls, so no signer/derivation/address detail
/// lives in the composition root (the ReconFactory discipline every other bridge follows).
///
/// Three things this resolves that the raw [`recon_client`] takes as given:
/// 1. **The L1 signer address.** `load_polymarket_creds_from` fills `address` from
///    `POLY_ADDRESS`/`POLY_FUNDER` — which on a deposit-wallet (`POLY_1271`) account is the FUNDER,
///    NOT the key's EOA. The CLOB `/auth/derive-api-key` ClobAuth signature must be made by, and
///    name, the EOA — so the address is re-derived from the private key here. (This is the same
///    signer-vs-funder split that made the Node probe in the workstreams spec's §P1.1 fail with
///    *"the order signer address has to be the address of the API KEY"*.)
/// 2. **The L2 trio.** [`ensure_l2`](crate::exec_plane::l1::ensure_l2) derives apiKey/secret/passphrase from the
///    L1 key — ONE blocking, proxy-routed network round-trip, performed at mount, exactly like the
///    deribit/ig/oanda/ctrader inline-recon handshakes. Failure ⇒ `None` (reconcile-inert, exec
///    unaffected), never a panic and never a mount failure.
/// 3. **The funder.** The data-api `/positions` key stays `POLY_FUNDER` (the deposit wallet whose
///    holdings are read), falling back to the EOA for a bare-key account.
///
/// The registry is FRESH (empty) — which is CORRECT for this entry point and is why it is not the
/// one a live mount uses: with no exec client on the other end there are no local coids to re-key
/// against, so order reports come back `client_order_id: None`. When exec IS mounted
/// (`flags.poly_exec`), `make_engine` calls [`crate::exec_plane::mount::live_mount_from_vars`] instead, which builds
/// both halves over ONE derivation and hands the exec thread's SHARED registry to [`recon_client`]
/// directly, so reports re-key to real coids.
pub fn recon_client_from_vars(
    vars: &std::collections::HashMap<String, String>,
) -> Option<Box<dyn ReconClient>> {
    recon_client_for_account(vars, &vike_model::account_keys::AccountLabel::Default)
}

/// [`recon_client_from_vars`] for ONE NAMED ACCOUNT.
///
/// Every credential AND the signature type are read through this account's own key names
/// (`crate::config::load_polymarket_creds_for_account`, [`signature_type_for_account`]) — there is
/// no path here on which a labelled account reads the default account's key, which is what would
/// otherwise reconcile one wallet's positions into another's books.
/// [`AccountLabel::Default`](vike_model::account_keys::AccountLabel::Default) is
/// [`recon_client_from_vars`], reached through it.
pub fn recon_client_for_account(
    vars: &std::collections::HashMap<String, String>,
    label: &vike_model::account_keys::AccountLabel,
) -> Option<Box<dyn ReconClient>> {
    use vike_bridge_core::credentials::Environment;

    // Polymarket is Polygon-MAINNET-only — there is no testnet — so the live tier is the only tier
    // (`load_polymarket_creds_from` also accepts the legacy `POLY_MAINNET_*` names). Absent
    // `POLY_PRIVATE_KEY` ⇒ `None`: absent-credentials-is-the-live-gate.
    let loaded = crate::config::load_polymarket_creds_for_account(Environment::Live, label, vars)?;
    let signer = match vike_bridge_core::eip712::eth_address_from_private_key(&loaded.private_key) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(target: "vike_polymarket::recon", error = %e, "polymarket recon unwired: POLY_PRIVATE_KEY is not a usable key");
            return None;
        }
    };
    // The funder (data-api positions key) is what the loader put in `address` (POLY_ADDRESS, else
    // POLY_FUNDER); `address` itself is re-pointed at the EOA for the ClobAuth signature.
    let funder = if loaded.address.is_empty() { signer.clone() } else { loaded.address.clone() };
    let mut creds = PolymarketCreds { address: signer, ..loaded };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if let Err(e) = crate::exec_plane::l1::ensure_l2(&mut creds, CLOB_BASE, now) {
        // Tunnel down / geo-blocked / rejected signature: stay reconcile-inert. NOT fatal — the
        // mount continues and exec is untouched (the deribit/ig/oanda connect-failure contract).
        tracing::warn!(target: "vike_polymarket::recon", error = %e, "polymarket recon unwired: L2 derivation failed (proxy down / geo-blocked?)");
        return None;
    }
    recon_client(creds, funder, signature_type_for_account(vars, label), PolymarketRegistry::new())
}

#[path = "recon_client_tests.rs"]
#[cfg(test)]
mod recon_client_tests;
