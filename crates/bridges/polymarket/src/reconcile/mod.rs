//! Polymarket `ReconcileClient` — the venue-facing report seam (`vike_execution::recon::ReconcileClient`) for a
//! PREDICTION MARKET. Polymarket is NOT a CEX, so the fit was investigated before building; the
//! finding is that all four report kinds map meaningfully, and the mapping is documented here so a
//! future reader understands what recon does — and, as importantly, does NOT — cover for an on-chain
//! settled venue.
//!
//! ## The four report kinds, and how a prediction market maps onto them
//! - **Positions** (`fetch_position_status_reports`): the data-api `GET /v2/positions?user=<funder>`
//!   read, every page of it (the SAME reader [`crate::settlement::positions`] uses for CTF auto-redeem; its
//!   module doc is the field map and says what the query must ask) — a per-outcome-token
//!   holding list. A Polymarket "position" is a balance of an ERC-1155 outcome `token_id` (you can
//!   only be LONG a token; to be "short YES" you hold the NO token, a DIFFERENT `token_id`), so every
//!   row is `qty >= 0`, keyed by `token_id`, `position_side = Both` — the spot/one-way convention,
//!   NOT a signed net perp position. Each row is a `Cash` FLOOR with NO price (the one spot constructor,
//!   the spot rule): the wallet's `average_price` is not the strategy's, so a token vike did not buy raises
//!   nothing and is never adopted, and only a wallet holding fewer than the book is held. This is the CORE
//!   reconcilable state and needs no auth (the funder address is the key). Account-wide (one read
//!   returns every holding), so — unlike the per-symbol IBKR/ctrader/alpaca clients — there is no
//!   conId filter and no synthesized flat row: `recon::diff::diff` iterates the venue rows and flags
//!   drift / external-only, and a local position they do not mention as `OrphanLocalPosition`, exactly
//!   as it does for the account-wide crypto-spot venues.
//! - **Balance** (`fetch_balance`): the CLOB `GET /balance-allowance?asset_type=COLLATERAL` read
//!   (L2-signed) — the account's USDC collateral, the venue "cash" `diff_balance` reconciles against.
//!   Base units are 6-decimal (USDC/CTF), so the wire value is divided by 1e6.
//! - **Fills** (`fetch_fill_reports`): the CLOB `GET /data/trades` history (L2-signed), decoded with
//!   the SAME registry re-keying + composite `trade_id` the live user-WS path
//!   ([`crate::execution::user_data::decode::decode_user`]) and the A3 resync ([`crate::execution::resync`]) use — a trade lists our
//!   order as `taker_order_id` (top-level size/price) or inside `maker_orders[]` (that entry's
//!   matched_amount/price); each is re-keyed CLOB→order client id via the shared [`OrderVenueIdTracker`] and
//!   stamped `trade_id = "{id}:{order_id}"`. That composite is load-bearing: it is the exact string
//!   the live path records in `seen_trade_ids`, so a fill both paths saw dedups (no false
//!   `MissingFill`) while a genuinely-missed fill still surfaces. Only the fillable statuses
//!   (`MATCHED`/`MINED`/`CONFIRMED`) count — the same set the live decode folds on.
//! - **Orders** (`fetch_order_status_reports`): the CLOB `GET /data/orders` active-order list
//!   (L2-signed), re-keyed CLOB→order client id via the registry so `recon::diff::diff` can match each against
//!   local by `order_client_id` (Polymarket assigns the order `id`; there is NO client-id echo — the
//!   registry is the only bridge back to the order client id, exactly as on the live path). An order the
//!   registry does not know (externally placed, or a cold registry after a restart) carries
//!   `order_client_id = None` → an ordinary `UnknownOrder`, the same fail-soft every venue uses. The
//!   active-order status is derived from fill progress (see [`normalize_order_status`]).
//!
//! ## The on-chain-settlement boundary — and the watcher that now covers it
//! Polymarket settlement is a two-step on-chain flow — a market RESOLVES, then winning tokens are
//! REDEEMED for USDC — and neither step produces a CLOB trade. The position report leaves a leg out
//! from RESOLUTION on ([`parse_position_reports`] skips a `redeemable` row: a settlement claim, not a
//! position), so a resolved leg the local book holds reads as `OrphanLocalPosition`, which folds
//! nothing, while the wallet still lists another holding, and as nothing at all when it lists none
//! (`recon::diff::diff` sweeps only a non-empty report, the same as every account-wide venue). The
//! redeem then moves cash WITHOUT a `/data/trades` fill, a `BalanceDrift` until local folds it.
//! Driving settlement is the job of the crate's
//! separate [`crate::settlement::resolve`] / [`crate::settlement::auto_redeem`] pollers, which fold the settlement fill that
//! then flattens the local position too; this seam is the periodic divergence net, complementary to
//! them, not a replacement.
//!
//! **[`crate::settlement::chain`] closes that boundary from the other side**, opt-in and default-off. Given a
//! [`ChainOracle`](crate::settlement::chain::ChainOracle) — attached with
//! [`ReconcileClientPolymarket::with_chain_oracle`] — [`fetch_fill_reports`](ReconcileClient::fetch_fill_reports)
//! APPENDS one settlement `FillReport` per on-chain redemption of this account's own tokens, read
//! straight from Polygon (`PayoutRedemption` joined to the funder's ERC-1155 outflow). Each carries
//! the SAME `trade_id` [`crate::settlement::resolve::settlement_trade_id`] stamps, which is the whole point:
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
//! This client is wired by [`crate::adapter::VenueAdapterPolymarket`], behind a
//! venue-local opt-in ON TOP of the workspace master gate — BOTH, and the start reads both
//! ([`crate::adapter`]'s `polymarket_reconcile_wanted`). The master gate is ON by default for a
//! start that arms a live venue account
//! (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`), so on an account that
//! runs, `flags.poly_reconcile` starts authenticated Polygon-MAINNET reconcile passes against this
//! real-money venue. ⚠ It starts nothing alone: the account runs only with `flags.poly_exec` on
//! (see [`polymarket_reconcile_enabled`]). The venue gate is still an act no other venue needs;
//! `flags.reconcile_off` refuses the outer one. The thing to read before enabling it:
//!
//! **The redeem blind spot** — a resolved market's redeem moves position → cash with NO
//! `/data/trades` fill, so a hybrid policy that auto-folded position divergences would be folding
//! a settlement it cannot see the other half of. `quarantine` puts an operator in front of
//! exactly that decision. This one is now ADDRESSABLE: wire [`crate::settlement::chain`]'s oracle
//! (a root that starts the chain watcher and hands this client its oracle) and the redeem arrives
//! as an explained `MissingFill` instead.
//!
//! With `flags.poly_reconcile` off — the default — the start builds no reconcile client and makes
//! no reconcile call, and the same is true with it on for a start whose reconcile gate answered no.
//!
//! ## Wire shapes are documented, the PARSERS are the proven deliverable
//! Every mapping is a PURE free function (`parse_*`) over the raw JSON, fixture-tested offline in
//! `tests/offline/polymarket_reconcile_parse.rs` (synthetic bodies — no network). The trade/order field
//! names are the ones [`crate::execution::user_data::decode`] already decodes live; the `/data/orders` and
//! `/balance-allowance` shapes are the documented Polymarket CLOB shapes, and `/v2/positions` is a real
//! body of a public wallet (`tests/data/`). Polymarket is
//! US-geo-blocked, so the `#[ignore]`d `tests/polymarket_smokes/polymarket_reconcile_smoke.rs` live-verifies the
//! fetch+parse read-only through the Dublin proxy (self-skips without creds). The status vocabulary
//! is the ONE FSM vocabulary (`OrderStatus`) the exec path already speaks.

// An operator's READ of the account (balance with allowances, open orders): asked, never folded.
pub(crate) mod venue_read;

use std::sync::Arc;

use serde_json::Value;
use vike_bridge_core::json::get_str_borrowed;
use vike_bridge_core::side::sign_default_buy;
use vike_execution::recon::ReconcileClient;
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{FillReport, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport};
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::VenueUrls;

use crate::config::{PolymarketConfig, first_token, row_or_not_running};
use crate::execution::order::SignatureType;
use crate::execution::registry::OrderVenueIdTracker;
use crate::execution::rest::{get_orders, get_signed, get_trades};
use crate::execution::user_data::decode::is_fillable_status;
use crate::settlement::chain::{ChainOracle, ChainSettlement};
use crate::settlement::positions::{Position, parse_positions, read_whole_wallet};

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
pub(crate) fn number(v: &Value, key: &str) -> f64 {
    v.get(key).map(number_val).unwrap_or(0.0)
}

/// A single JSON value → `f64`, tolerating both the number and the quoted-string encodings.
fn number_val(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0.0)
}

/// The array of rows from a Polymarket list response — a bare `[..]`, or wrapped under a `data` key
/// (the paginated `{data:[..], next_cursor, ..}` shape some CLOB list endpoints use). `None` when the
/// body is neither.
pub(crate) fn rows(v: &Value) -> Option<&Vec<Value>> {
    v.as_array().or_else(|| v.get("data").and_then(|d| d.as_array()))
}

/// A trade row's timestamp (epoch) → `i64`, tolerating the field-name variants the CLOB uses across
/// the WS frame (`timestamp`) and the `/data/trades` REST rows (`match_time`); `last_update` is a
/// final fallback. `0` when none is present (ts is informational — dedup keys on `trade_id`).
fn trade_ts(t: &Value) -> i64 {
    for k in ["match_time", "timestamp", "last_update"] {
        let x = number(t, k);
        if x != 0.0 {
            return x as i64;
        }
    }
    0
}

/// Map a Polymarket CLOB active order onto the FSM [`OrderStatus`]. `/data/orders` lists ACTIVE
/// (resting) orders, so status is derived from fill progress (`size_matched`,
/// [`OrderStatus::from_fill_progress`], the one split of every venue whose resting status carries no
/// distinct partial value): any match → `PartiallyFilled`, the whole `original_size` included,
/// because the row still rests (B-16, the owner's ruling); untouched → `Accepted`. A raw status
/// NAMING a cancel (defensive — the active list should not carry one) → `Canceled`. No venue word
/// passes through, so no word can be misread.
pub fn normalize_order_status(raw: &str, matched: f64) -> OrderStatus {
    if raw.to_ascii_uppercase().contains("CANCEL") {
        return OrderStatus::Canceled;
    }
    OrderStatus::from_fill_progress(matched)
}

// --- pure parsers (fixture-tested over the raw JSON bodies) --------------------------------------

/// `GET /data/orders` (a bare array, or `{data:[..]}`) → `OrderStatusReport`. Each row is re-keyed
/// CLOB `id` → order client id via `lookup` (the shared registry's `lookup_clob`); an order the registry does
/// not know maps to `order_client_id: None` (an externally-placed / cold-registry order →
/// `UnknownOrder`, never a panic). `asset_id` → symbol (the outcome `token_id`); `original_size` →
/// qty; `order_type` is always `OrderType::Limit` (the row's `order_type` is the TIF, see the field);
/// `size_matched` → filled_qty; `avg_px` is `0.0` (the active-orders row carries the LIMIT
/// price, not an average FILL price, and the diff does not read order `avg_px`); that `price` → price; status via
/// [`normalize_order_status`]; `created_at` → ts.
pub fn parse_order_reports(
    v: &Value,
    lookup: impl Fn(&str) -> Option<(String, i32)>,
) -> Result<Vec<OrderStatusReport>, String> {
    let rows = rows(v).ok_or("expected an orders array")?;
    Ok(rows
        .iter()
        .map(|o| {
            let clob_id = get_str_borrowed(o, "id").to_string();
            let original = number(o, "original_size");
            let matched = number(o, "size_matched");
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: get_str_borrowed(o, "asset_id").to_string(),
                order_client_id: lookup(&clob_id).map(|(order_client_id, _)| order_client_id),
                venue_order_id: clob_id.into(),
                side: sign_default_buy(get_str_borrowed(o, "side")),
                // Always "limit": every CLOB order is a signed LIMIT at `price` (a market order is a
                // marketable FOK limit). The row's own `order_type` field is the TIME-IN-FORCE
                // (`GTC`/`FOK`/`GTD`, the words `client.rs`'s `order_type_of` sends), and an
                // `OrderStatusReport` has no TIF field, so it is not reported at all — read as an
                // order kind it adopted every Polymarket order as `OrderType::Unrecognized("gtc")`.
                order_type: OrderType::Limit,
                qty: original,
                filled_qty: matched,
                average_price: 0.0,
                status: normalize_order_status(get_str_borrowed(o, "status"), matched),
                timestamp: number(o, "created_at") as i64,
                // the LIMIT price the order is signed at (a share of the outcome, 0 to 1)
                price: o
                    .get("price")
                    .and_then(vike_bridge_core::json::json_number)
                    .filter(|p| p.is_finite() && *p > 0.0),
                trigger_price: None,
                trail: None,
                trail_percent: None,
            }
        })
        .collect())
}

/// `GET /data/trades` (a bare array, or `{data:[..]}`) → `FillReport`, ONE per order of ours the
/// trade matched. A trade lists our order as `taker_order_id` (top-level size/price →
/// `LiquiditySide::Taker`) or inside `maker_orders[]` (that entry's `matched_amount`/`price` →
/// `LiquiditySide::Maker`); each is re-keyed CLOB→order client id via `lookup`, and rows for NO order of ours
/// emit nothing (not our fill to fold — the SAME contract as `user_websocket::decode_user` /
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
        if !is_fillable_status(get_str_borrowed(t, "status")) {
            continue;
        }
        let trade_id = get_str_borrowed(t, "id").to_string();
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
        let asset_id = get_str_borrowed(t, "asset_id").to_string();
        let ts = trade_ts(t);
        // taker side: our order == taker_order_id → top-level size/price.
        let taker = get_str_borrowed(t, "taker_order_id").to_string();
        if let (Some((order_client_id, side)), Some(tid)) =
            (lookup(&taker), composite_trade_id(&trade_id, &taker))
        {
            out.push(fill_report(
                tid,
                &taker,
                &asset_id,
                order_client_id,
                side,
                number(t, "size"),
                number(t, "price"),
                LiquiditySide::Taker,
                ts,
            ));
        }
        // maker side: each maker_orders[].order_id that is ours → that entry's matched_amount/price.
        for mo in t.get("maker_orders").and_then(|m| m.as_array()).into_iter().flatten() {
            let oid = get_str_borrowed(mo, "order_id").to_string();
            if let (Some((order_client_id, side)), Some(tid)) =
                (lookup(&oid), composite_trade_id(&trade_id, &oid))
            {
                out.push(fill_report(
                    tid,
                    &oid,
                    &asset_id,
                    order_client_id,
                    side,
                    number(mo, "matched_amount"),
                    number(mo, "price"),
                    LiquiditySide::Maker,
                    ts,
                ));
            }
        }
    }
    Ok(out)
}

/// The composite id BOTH this parser and the live `crate::execution::user_data::decode` decode stamp:
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

fn fill_report(
    trade_id: TradeId,
    order_clob_id: &str,
    asset_id: &str,
    order_client_id: String,
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
        order_client_id: Some(order_client_id),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new(),
        liquidity_side,
        timestamp: ts,
    }
}

/// One page of the data-api's `GET /v2/positions` → `PositionStatusReport`
/// ([`crate::settlement::positions::parse_positions`] is the one parser), with every leg
/// of a RESOLVED market left out (`position_reports`).
pub fn parse_position_reports(v: &Value) -> Result<Vec<PositionStatusReport>, String> {
    position_reports(&parse_positions(v)?.positions)
}

/// The wallet's rows → the reports reconcile diffs, SKIPPING every row whose `redeemable` is `true`;
/// a row where it is absent or `false` is reported as before.
///
/// `redeemable` is read here as what it is, a RESOLUTION flag (`Position::redeemable`): the row's
/// condition has resolved, losers included, so it says nothing about the payout and nothing here asks
/// it to. A leg of a resolved market is a settlement claim, owned by
/// [`crate::settlement::resolve`] and [`crate::settlement::auto_redeem`], not a
/// position the engine can trade: its market is closed, so a `PositionOnlyExternal` raised for it can
/// never be confirmed (the admit step finds no instrument: Gamma lists open markets only) and every
/// pass and every restart raised it again (measured on the live account 2026-11-10: the four resolved
/// legs `crate::settlement::resolve`'s module doc names, three losers and one winner).
/// The cost, stated: a resolved leg the LOCAL book still holds leaves the report at resolution, so it
/// reads as `OrphanLocalPosition` (no policy folds it) while the wallet lists another holding, and as
/// nothing at all when it lists none.
fn position_reports(positions: &[Position]) -> Result<Vec<PositionStatusReport>, String> {
    positions.iter().filter(|p| !p.redeemable).map(position_report).collect()
}

/// One held position → its report, through the one spot constructor (`PositionStatusReport::spot_holding`):
/// the token (`token_id`) → symbol; the holding (`current_size`, always `>= 0` — a Polymarket holding is
/// one-way, fully funded) → qty; `Cash`, `position_side = Both` (matching the `"BOTH"` the live fills fold
/// under). Zero-size rows are KEPT when present — they let the diff detect a stale local position the venue
/// now reports flat. A `Cash` row is a FLOOR (`vike_execution::recon::diff`) with NO price: the wallet's
/// `average_price` is not the strategy's, so a token vike did not buy raises nothing and neither `synthesize`
/// nor a confirm adopts it, and a wallet holding fewer tokens than the book is held and shrinks at the
/// book's own average on confirm (the spot rule, owner 2026-11-11).
///
/// `Err`, naming the token, for a row that states no entry price: the wire requires the field and the
/// venue's rule is that a missing number is unavailable, never zero, so a row without one means the venue
/// changed its shape, and the pass says so (a shape check: the figure itself is not reported).
fn position_report(p: &Position) -> Result<PositionStatusReport, String> {
    p.average_price.ok_or_else(|| {
        format!(
            "polymarket's positions row for token {} states no readable avg_price, so its entry \
             price is unknown",
            p.asset
        )
    })?;
    // step 0.0: the wallet's own figure, dust included (this layer knows no share step)
    Ok(PositionStatusReport::spot_holding(VENUE, &p.asset, p.size, 0.0, 0))
}

/// One observed on-chain settlement → the `FillReport` that EXPLAINS it to reconcile.
///
/// The `trade_id` is [`crate::settlement::resolve::settlement_trade_id`]'s exact string, so a settlement the
/// resolve poller already folded dedups against `local.seen_trade_ids` and produces no divergence at
/// all; one it has not folded surfaces as a `MissingFill` an operator (or `hybrid`) can apply. The
/// fill CLOSES a long — `side = -1`, `last_qty` = the redeemed shares, `last_price` = the collateral
/// per token the chain actually paid — mirroring `resolve::settlement_fill`'s construction so both
/// paths book the identical realised PnL. `venue_order_id`/`order_client_id` carry the
/// `resolution:<conditionId>` tag (a settlement has no order behind it), and `liquidity_side` is
/// `Unknown`: a settlement is neither maker nor taker.
pub fn settlement_fill_report(s: &ChainSettlement) -> FillReport {
    FillReport {
        venue: VENUE.to_string(),
        symbol: s.token_id.clone(),
        trade_id: crate::settlement::resolve::settlement_trade_id(&s.condition_id, &s.token_id),
        venue_order_id: format!("resolution:{}", s.condition_id).into(),
        order_client_id: None,
        side: -1,
        last_qty: s.qty,
        last_price: s.price,
        commission: 0.0,
        commission_asset: String::new(),
        liquidity_side: LiquiditySide::Unknown,
        timestamp: s.timestamp_ms,
    }
}

/// CLOB `GET /balance-allowance?asset_type=COLLATERAL` → the `balance` (USDC collateral) in whole
/// USDC (the wire value is 6-decimal base units, so it is divided by 1e6). `None` when the field is
/// absent (fail-soft); a non-object body is a hard `Err`, never a panic.
pub fn parse_balance(v: &Value) -> Result<Option<f64>, String> {
    if !v.is_object() {
        return Err("expected a balance-allowance object".to_string());
    }
    Ok(v.get("balance").map(number_val).map(|b| b / USDC_DECIMALS))
}

// --- the client ---------------------------------------------------------------------------------

/// One account-wide Polymarket reconcile client. Holds its OWN [`PolymarketConfig`] (the L2 trio for
/// the signed order/fill/balance reads), the funder address (the data-api positions key), the
/// account's [`SignatureType`] (the balance-allowance cache is keyed by it), and a clone of the
/// shared [`OrderVenueIdTracker`] — the SAME map the exec thread writes and the user-WS pump reads, so
/// order/fill reports re-key CLOB→order client id identically to the live path. `ReconcileClient`'s methods take
/// `&self`; the registry is `Arc`-backed and the `ureq` agent is `Send + Sync`, so no interior
/// mutability is needed.
pub struct ReconcileClientPolymarket {
    creds: PolymarketConfig,
    clob_base: String,
    data_api: String,
    funder: String,
    signature_type: SignatureType,
    registry: OrderVenueIdTracker,
    limit: u32,
    /// OPTIONAL on-chain settlement oracle ([`crate::settlement::chain`]) — `None` (the default) leaves every
    /// fetch below byte-identical to the CLOB-only client.
    chain: Option<Arc<ChainOracle>>,
}

impl ReconcileClientPolymarket {
    /// Build a reconcile client against the CLOB at `clob_base` and the data-api at `data_api` (the
    /// venue's rows, `crate::config`'s `clob_base` and `data_api_base`). `funder` is the
    /// data-api positions key (the deposit-wallet / proxy / EOA address whose holdings are read);
    /// `signature_type` matches the exec wallet (the balance-allowance cache is per-signature-type);
    /// pass the SAME `registry` clone the exec client ([`crate::execution::spawn_live`]) and user-WS
    /// pump share so orders/fills re-key to the live order client ids.
    pub fn new(
        creds: PolymarketConfig,
        funder: String,
        signature_type: SignatureType,
        registry: OrderVenueIdTracker,
        clob_base: String,
        data_api: String,
    ) -> Self {
        ReconcileClientPolymarket {
            creds,
            clob_base,
            data_api,
            funder,
            signature_type,
            registry,
            limit: DEFAULT_FETCH_LIMIT,
            chain: None,
        }
    }

    /// Attach the on-chain settlement oracle: from here on `fetch_fill_reports` also reports the
    /// redemptions it has observed (see the module doc's on-chain-settlement section). Opt-in — the
    /// caller builds an oracle only when it starts the chain watcher
    /// ([`crate::settlement::chain::ChainWatchPoller::spawn`]).
    pub fn with_chain_oracle(mut self, oracle: Arc<ChainOracle>) -> Self {
        self.chain = Some(oracle);
        self
    }

    /// The chain-derived half of [`fetch_fill_reports`](ReconcileClient::fetch_fill_reports): the
    /// settlements observed at or after `since`, as `FillReport`s. Empty without an oracle.
    fn chain_settlement_fills(&self, since: i64) -> Vec<FillReport> {
        let Some(oracle) = self.chain.as_ref() else { return Vec::new() };
        oracle.settlements_since(since).iter().map(settlement_fill_report).collect()
    }
}

impl ReconcileClient for ReconcileClientPolymarket {
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

    /// The funder's account-wide token holdings (data-api `/v2/positions`, every page, unauthenticated
    /// — the funder address is the key). Account-wide, so an absent token is simply not held: no
    /// flat-row synthesis, and a local position the venue does not list reads as
    /// `OrphanLocalPosition` (`crates/vike-execution/src/recon/diff.rs`'s `diff`). That is why any failure
    /// here is an `Err` and never a shorter list: `run_pass` then folds nothing. A leg of a resolved
    /// market is left out on purpose (`position_reports`).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        position_reports(&read_whole_wallet(
            &self.data_api,
            &self.funder,
            crate::transport::REST_TIMEOUT,
        )?)
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

/// The venue → `ReconcileClient` factory (the ReconFactory seam) for Polymarket. `None` when the L2 trio
/// is absent (`api_key`/`secret` empty) — the order/fill/balance reads are L2-signed, so without it
/// there is nothing to reconcile with; reconcile stays unwired and exec is unaffected (the same
/// absent-creds-is-the-live-gate discipline every bridge follows). Construction is otherwise
/// infallible (no network), so a fully-credentialed call always returns `Some`.
///
/// **Wired by [`crate::adapter::VenueAdapterPolymarket`]** — through the
/// [`reconcile_client_for_account`] convenience below, which does the L1→L2 derivation this raw factory
/// takes as already-done. Call THIS one when you already hold derived L2 creds and the SAME
/// registry the exec client writes (the live-mount path); call [`reconcile_client_for_account`] from a
/// caller that only holds the account's credential fields.
///
/// `urls` are the venue's address rows; a missing CLOB or data-api row is `None` with the refusal's
/// words at `error!`.
pub fn recon_client(
    creds: PolymarketConfig,
    funder: String,
    signature_type: SignatureType,
    registry: OrderVenueIdTracker,
    urls: &VenueUrls,
) -> Option<Box<dyn ReconcileClient>> {
    if creds.api_key.expose().is_empty() || creds.secret.expose().is_empty() {
        tracing::warn!(target: "vike_polymarket::recon", "Polymarket recon client unwired: no L2 creds");
        return None;
    }
    let what = "polymarket reconcile";
    let clob = row_or_not_running(what, crate::config::clob_base(urls))?.to_string();
    let data_api = row_or_not_running(what, crate::config::data_api_base(urls))?.to_string();
    Some(Box::new(ReconcileClientPolymarket::new(
        creds,
        funder,
        signature_type,
        registry,
        clob,
        data_api,
    )))
}

/// The SECOND gate on Polymarket reconciliation, on top of the workspace-wide master gate: the
/// EXACT string `"1"` in the caller's map, under the name the daemon folds `flags.poly_reconcile`
/// into (`vike_config::flags::POLY_RECONCILE_ENV`, the one owner of that name), default OFF. The
/// start requires BOTH (`crate::adapter`'s `polymarket_reconcile_wanted`) — this one decides whether the
/// VENUE wants a read client, the master gate whether this process starts the driver that would
/// use one.
///
/// ⚠ **The master gate is ON by default** for any start that arms a live venue account
/// (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`), so on an account that
/// runs, this flag starts authenticated Polygon-MAINNET passes against a real-money venue with no
/// testnet. The refusals are `flags.poly_reconcile` off (this venue only) and
/// `flags.reconcile_off` (every venue).
///
/// ⚠ **It starts nothing alone.** The account runs only with `flags.poly_exec` on:
/// `crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `contract_parts` does not ask a bridge
/// to start an account its `resolve` does not arm, and drops the reconcile handle of a start whose
/// exec did not come up (decision 0156). So there is no reconcile-only start, and no paper engine
/// is ever diffed against this account.
///
/// **The policy.** With both flags on, the start builds both halves from ONE
/// `live_runtime_for_account` call over one shared `OrderVenueIdTracker`, so a local order really
/// does exist at the venue and re-keys to its order client id, and `PositionDrift` converges two views of
/// the SAME account rather than importing one into the other — so `hybrid` is sound. (One
/// residual: a resolved market moves position → cash with NO `/data/trades` fill. The report drops
/// the leg at RESOLUTION (`position_reports`), so a locally held one reads as
/// `OrphanLocalPosition`, which `hybrid` holds and no policy folds, or as nothing when the wallet
/// lists no other holding; the redeem's cash is a `BalanceDrift`. The chain watcher's oracle, handed
/// to this client, is what turns the redeem into an explained `MissingFill` — and no composition
/// root starts that watcher today.)
///
/// Unset (the default) ⇒ the start builds no reconcile client and makes no reconcile call.
///
/// Map-only since decision 0095: `flags.poly_reconcile` is a settings row the daemon folds into
/// this map, and a `POLY_RECONCILE` variable refuses startup (`vike_config::REMOVED_ENV`).
pub fn polymarket_reconcile_enabled(vars: &std::collections::HashMap<String, String>) -> bool {
    vars.get(vike_config::flags::POLY_RECONCILE_ENV).map(|v| first_token(v)) == Some("1")
}

/// The account field holding the wallet's signature type — `vike_venues::credential_schema`'s
/// polymarket row's word.
const SIGNATURE_TYPE: &str = "SIGNATURE_TYPE";

/// The account's `SIGNATURE_TYPE` field → [`SignatureType`], defaulting to `Polymarket1271` (the
/// deposit-wallet account type this repo's live account uses). Mirrors the same parse in
/// [`crate::execution`]'s live-mount helper and in the reconcile smoke, kept here so the start path and
/// the smoke agree by construction.
///
/// ⚠ **Per-ACCOUNT, not per-deployment, and the reason is that it is a property of the WALLET.** A
/// bare-key EOA and a deposit wallet sign differently, and two Polymarket accounts on one box may
/// be one of each — a labelled account inheriting the default account's type would sign its orders
/// in a shape the venue rejects (or, worse, name the wrong maker). It is read out of THIS
/// account's fields, so it cannot be another account's.
///
/// ⚠ **No fallback to another account**: an account with no `SIGNATURE_TYPE` field reads the
/// `Polymarket1271` DEFAULT — the type's own default, never the other account's value.
pub fn signature_type_for_account(creds: &AccountCredentials<'_>) -> SignatureType {
    match creds.get(SIGNATURE_TYPE).map(first_token) {
        Some("0") => SignatureType::Eoa,
        Some("1") => SignatureType::PolymarketProxy,
        Some("2") => SignatureType::PolymarketGnosisSafe,
        _ => SignatureType::Polymarket1271,
    }
}

/// Build the reconcile client straight from the started ACCOUNT's credential fields — the
/// recon-only seam [`crate::adapter::VenueAdapterPolymarket`] calls, so no
/// signer/derivation/address detail lives in the composition root (the ReconFactory discipline
/// every other bridge follows).
///
/// Every credential AND the signature type are read out of this account's own fields
/// (`crate::config::load_polymarket_credentials_from`, [`signature_type_for_account`]) — there is no path
/// here on which a labelled account reads the default account's key, which is what would otherwise
/// reconcile one wallet's positions into another's books.
///
/// Three things this resolves that the raw [`recon_client`] takes as given:
/// 1. **The L1 signer address.** `load_polymarket_credentials_from` fills `address` from the account's
///    `ADDRESS`/`FUNDER` fields — which on a deposit-wallet (`POLY_1271`) account is the FUNDER,
///    NOT the key's EOA. The CLOB `/auth/derive-api-key` ClobAuth signature must be made by, and
///    name, the EOA — so the address is re-derived from the private key here. (This is the same
///    signer-vs-funder split that made the Node probe in the workstreams spec's §P1.1 fail with
///    *"the order signer address has to be the address of the API KEY"*.)
/// 2. **The L2 trio.** [`ensure_l2`](crate::transport::sign::l1::ensure_l2) derives apiKey/secret/passphrase from the
///    L1 key — ONE blocking, proxy-routed network round-trip, performed at start, exactly like the
///    deribit/ig/oanda/ctrader inline-recon handshakes. Failure ⇒ `None` (reconcile-inert, exec
///    unaffected), never a panic and never a start failure.
/// 3. **The funder.** The data-api `/v2/positions` key stays the account's funder address (the
///    deposit wallet whose holdings are read), falling back to the EOA for a bare-key account.
///
/// The registry is FRESH (empty) — which is CORRECT for this entry point and is why it is not the
/// one a live start uses: with no exec client on the other end there are no local order client ids to re-key
/// against, so order reports come back `order_client_id: None`. When exec IS started
/// (`flags.poly_exec`), the start calls [`crate::adapter::live_runtime_for_account`]
/// instead, which builds both halves over ONE derivation and hands the exec thread's SHARED
/// registry to [`recon_client`] directly, so reports re-key to real order client ids.
///
/// `urls` are the venue's address rows (`AdapterInputs::settings`); a missing row is `None` with the
/// refusal's words, before any network.
pub fn reconcile_client_for_account(
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
) -> Option<Box<dyn ReconcileClient>> {
    // Polymarket is Polygon-MAINNET-only — there is no testnet — so the live tier is the only tier.
    // An absent `PRIVATE_KEY` field ⇒ `None`: absent-credentials-is-the-live-gate.
    let loaded = crate::config::load_polymarket_credentials_from(creds)?;
    let signer = match vike_bridge_core::eip712::eth_address_from_private_key(
        loaded.private_key.expose(),
    ) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(target: "vike_polymarket::recon", error = %e, "polymarket recon unwired: the account's PRIVATE_KEY is not a usable key");
            return None;
        }
    };
    // The funder (data-api positions key) is what the loader put in `address` (the account's
    // ADDRESS, else its FUNDER); `address` itself is re-pointed at the EOA for the ClobAuth signature.
    let funder = if loaded.address.is_empty() { signer.clone() } else { loaded.address.clone() };
    let mut l2 = PolymarketConfig { address: signer, ..loaded };
    let clob = row_or_not_running("polymarket reconcile", crate::config::clob_base(urls))?;
    let now = vike_model::time::clock::now_secs();
    if let Err(e) = crate::transport::sign::l1::ensure_l2(&mut l2, clob, now) {
        // Tunnel down / geo-blocked / rejected signature: stay reconcile-inert. NOT fatal — the
        // start continues and exec is untouched (the deribit/ig/oanda connect-failure contract).
        tracing::warn!(target: "vike_polymarket::recon", error = %e, "polymarket recon unwired: L2 derivation failed (proxy down / geo-blocked?)");
        return None;
    }
    recon_client(l2, funder, signature_type_for_account(creds), OrderVenueIdTracker::new(), urls)
}

#[path = "reconcile_tests.rs"]
#[cfg(test)]
mod reconcile_tests;
