//! `chain`'s pure log decoders: the settlement events and the Phase C shared decode library.

use serde_json::Value;

use super::abi::{
    data_words, dyn_array, dyn_array_ids, log_str, log_topic, log_u64, u256_word_to_decimal,
    word_address, word_u128,
};
use super::{
    RedeemVenue, Redemption, ResolutionEvent, TOPIC_CONDITION_RESOLUTION,
    TOPIC_CTF_PAYOUT_REDEMPTION, TOPIC_NEG_RISK_PAYOUT_REDEMPTION, TOPIC_TRANSFER_BATCH,
    TOPIC_TRANSFER_SINGLE, TokenTransfer, USDC_DECIMALS,
};
use crate::exec_plane::order::Side;

// ---------------------------------------------------------------------------------------------
// Pure decoders (fixture-tested against REAL mainnet logs)
// ---------------------------------------------------------------------------------------------

/// CTF `PayoutRedemption` log → [`Redemption`]. Topics: `[topic0, redeemer, collateralToken,
/// parentCollectionId]`; data head is `[conditionId, offset(indexSets), payout]` with the array in
/// the tail — so the offset is ALWAYS `0x60` (three head words), which every one of the 244 live
/// logs sampled confirmed and which this decoder re-checks through [`dyn_array`].
pub fn decode_ctf_redemption(l: &Value) -> Result<Redemption, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_CTF_PAYOUT_REDEMPTION) {
        return Err(format!("not a CTF PayoutRedemption log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(Redemption {
        venue: RedeemVenue::Ctf,
        redeemer: word_address(&log_topic(l, 1)?)?,
        condition_id: format!("0x{}", w.first().ok_or("no conditionId word")?),
        payout_usdc: word_u128(w.get(2).ok_or("no payout word")?)? as f64 / USDC_DECIMALS,
        slot_values: dyn_array(&w, 1)?,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: (log_u64(l, "blockTimestamp") as i64).saturating_mul(1000),
    })
}

/// NegRiskAdapter `PayoutRedemption` log → [`Redemption`]. Topics: `[topic0, redeemer, conditionId]`
/// (the conditionId identity is proven in the module doc); data head is `[offset(amounts), payout]`
/// ⇒ offset `0x40`, again re-checked structurally.
pub fn decode_neg_risk_redemption(l: &Value) -> Result<Redemption, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_NEG_RISK_PAYOUT_REDEMPTION) {
        return Err(format!("not a NegRiskAdapter PayoutRedemption log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(Redemption {
        venue: RedeemVenue::NegRisk,
        redeemer: word_address(&log_topic(l, 1)?)?,
        condition_id: log_topic(l, 2)?,
        payout_usdc: word_u128(w.get(1).ok_or("no payout word")?)? as f64 / USDC_DECIMALS,
        slot_values: dyn_array(&w, 0)?,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: (log_u64(l, "blockTimestamp") as i64).saturating_mul(1000),
    })
}

/// Either redemption shape, dispatched on `topic0`. `None` for any other log.
pub fn decode_redemption(l: &Value) -> Option<Redemption> {
    match log_topic(l, 0).ok()?.to_ascii_lowercase() {
        t if t == TOPIC_CTF_PAYOUT_REDEMPTION => decode_ctf_redemption(l).ok(),
        t if t == TOPIC_NEG_RISK_PAYOUT_REDEMPTION => decode_neg_risk_redemption(l).ok(),
        _ => None,
    }
}

/// CTF `ConditionResolution` log → [`ResolutionEvent`]. Topics: `[topic0, conditionId, oracle,
/// questionId]`; data head is `[outcomeSlotCount, offset(payoutNumerators)]` ⇒ offset `0x40`.
pub fn decode_condition_resolution(l: &Value) -> Result<ResolutionEvent, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_CONDITION_RESOLUTION) {
        return Err(format!("not a ConditionResolution log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(ResolutionEvent {
        condition_id: log_topic(l, 1)?,
        oracle: word_address(&log_topic(l, 2)?)?,
        question_id: log_topic(l, 3)?,
        outcome_slot_count: word_u128(w.first().ok_or("no outcomeSlotCount word")?)?,
        payout_numerators: dyn_array(&w, 1)?,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// ERC-1155 `TransferSingle`/`TransferBatch` → [`TokenTransfer`], the single form normalised into
/// one-element vectors so the caller has ONE shape to reason about.
pub fn decode_token_transfer(l: &Value) -> Result<TokenTransfer, String> {
    let t0 = log_topic(l, 0)?.to_ascii_lowercase();
    let w = data_words(&log_str(l, "data"));
    let (ids, values) = if t0 == TOPIC_TRANSFER_SINGLE {
        (
            vec![u256_word_to_decimal(w.first().ok_or("no id word")?)?],
            vec![word_u128(w.get(1).ok_or("no value word")?)?],
        )
    } else if t0 == TOPIC_TRANSFER_BATCH {
        (dyn_array_ids(&w, 0)?, dyn_array(&w, 1)?)
    } else {
        return Err(format!("not an ERC-1155 transfer log (topic0 {t0})"));
    };
    if ids.len() != values.len() {
        return Err(format!("ids/values length mismatch ({} vs {})", ids.len(), values.len()));
    }
    Ok(TokenTransfer {
        operator: word_address(&log_topic(l, 1)?)?,
        from: word_address(&log_topic(l, 2)?)?,
        to: word_address(&log_topic(l, 3)?)?,
        ids,
        values,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// One `eth_call` result word → `u128` (the shape every view function here returns).
pub fn decode_uint_result(hex: &str) -> Result<u128, String> {
    let w = data_words(hex);
    word_u128(w.first().ok_or("empty eth_call result")?)
}

// ---------------------------------------------------------------------------------------------
// Phase C decoders — the shared decode library (drift unification, see the module doc). Pure,
// additive, and NOT consumed by [`ChainWatcher`]/[`ChainOracle`] below.
// ---------------------------------------------------------------------------------------------

/// `OrderFilled(bytes32,address,address,uint8,uint256,uint256,uint256,uint256,bytes32,bytes32)` on
/// the unified V2 CTF Exchange ([`V2_EXCHANGE`]) — the sole `OrderFilled` emitter since the
/// ~Apr-2026 V1→V2 migration. Mirrors `onchain_decode.py`'s `ORDERFILLED_TOPIC0`.
pub const TOPIC_ORDER_FILLED_V2: &str =
    "0xd543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee";
/// `OrderFilled(bytes32,address,address,uint256,uint256,uint256,uint256,uint256)` — the legacy
/// 8-param layout on [`V1_EXCHANGE_STD`]/[`V1_EXCHANGE_NEG`] (pre-~Apr-2026).
pub const TOPIC_ORDER_FILLED_V1: &str =
    "0xd0a08e8c493f9c94f29311604c9de1b4e8c8d4c06bd0c789af57f2d65bfec0f6";
/// CTF `PositionSplit(address,bytes32,bytes32,uint256[])` — mints a full outcome-token set against
/// collateral.
pub const TOPIC_CTF_POSITION_SPLIT: &str =
    "0x2e6bb91f8cbcda0c93623c54d0403a43514fabc40084ec96b6d5379a74786298";
/// CTF `PositionsMerge(address,bytes32,bytes32,uint256[])` — burns a full outcome-token set back
/// into collateral.
pub const TOPIC_CTF_POSITION_MERGE: &str =
    "0x6f13ca62553fcc2bcd2372180a43949c1e4cebba603901ede2f4e14f36b282ca";
/// NegRiskAdapter `PositionsConverted(address,bytes32,uint256,uint256)` — converts YES tokens
/// across a neg-risk market's outcome set. `indexSet` is a BITMAP over the outcome slots, NOT a
/// per-slot amount (see the memory note `polymarket-negrisk-set-facts`).
pub const TOPIC_NEG_RISK_POSITIONS_CONVERTED: &str =
    "0xb03d19dddbc72a87e735ff0ea3b57bef133ebe44e1894284916a84044deb367e";
/// Standard ERC-20 `Transfer(address,address,uint256)` — the universally-published constant,
/// reused here for the USDC.e collateral leg ([`crate::exec_plane::settlement::redeem::USDC_E_ADDRESS`]).
pub const TOPIC_USDC_TRANSFER: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// The unified V2 CTF Exchange — every market's `OrderFilled` emitter since the ~Apr-2026
/// migration (the same literal `onchain_decode.py` calls `EXCHANGE`/`NEGRISK_EXCHANGE`).
pub const V2_EXCHANGE: &str = "0xe111180000d2663c0091e4f400237545b87b996b";
/// V1 (legacy) CTF Exchange — standard/binary markets, pre-~Apr-2026.
pub const V1_EXCHANGE_STD: &str = "0x4bfb41d5b3570defd03c39a9a4d8de6bd8b8982e";
/// V1 (legacy) NegRisk CTF Exchange, pre-~Apr-2026.
pub const V1_EXCHANGE_NEG: &str = "0xc5d563a36ae78145c45a50134d48a1215220f80a";

/// Which `OrderFilled` ABI generation decoded a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderFillAbi {
    /// The 10-param layout on the unified [`V2_EXCHANGE`] — `side`-tagged, single `tokenId`.
    V2,
    /// The 8-param legacy layout on [`V1_EXCHANGE_STD`]/[`V1_EXCHANGE_NEG`] —
    /// `makerAssetId`/`takerAssetId`, side inferred from which asset id is the zero (USDC) leg.
    V1,
}

/// Which side of the fill this row is — mirrors the Python daemon's MAKER-ONLY row model
/// (`onchain_decode.py`'s module doc): the indexed `maker` is always the order owner, and
/// recording the taker leg too would double-count a mint-matched trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRole {
    /// The on-chain `taker` topic IS the exchange contract itself — this maker's order aggressed.
    Taker,
    /// The on-chain `taker` topic is a real counter-wallet — this maker's resting order was hit.
    Maker,
}

/// One decoded `OrderFilled` fill, MAKER perspective — [`decode_order_fill_v2`]/
/// [`decode_order_fill_v1`] normalise both ABI generations into this one shape, mirroring
/// `onchain_decode.py`'s `decode_orderfilled`/`decode_orderfilled_v1` row.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderFill {
    pub abi: OrderFillAbi,
    pub order_hash: String,
    pub maker: String,
    pub taker: String,
    /// The ERC-1155 outcome token (decimal) — the vike `symbol` for this venue.
    pub token_id: String,
    pub side: Side,
    pub size: f64,
    pub price: f64,
    pub fee_usdc: f64,
    pub role: FillRole,
    pub tx_hash: String,
    pub block: u64,
    /// Block timestamp in ms when the RPC supplies `blockTimestamp` (most do), else `0`.
    pub ts_ms: i64,
}

/// V2 (10-param) `OrderFilled` on [`V2_EXCHANGE`]. Topics: `[topic0, orderHash, maker, taker]`;
/// data is `[side, tokenId, makerAmt, takerAmt, fee, builder, metadata]` — 7 fixed words, no
/// dynamic array. `side` 0 = BUY (maker pays USDC, gets tokens) / 1 = SELL (maker pays tokens,
/// gets USDC) — the same convention [`crate::exec_plane::order::Side::code`] encodes for order signing. A
/// zero-size fill is rejected, mirroring the Python decoder's `None` return.
pub fn decode_order_fill_v2(l: &Value) -> Result<OrderFill, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_ORDER_FILLED_V2) {
        return Err(format!("not a V2 OrderFilled log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    let side =
        if word_u128(w.first().ok_or("no side word")?)? == 0 { Side::Buy } else { Side::Sell };
    let token_id = u256_word_to_decimal(w.get(1).ok_or("no tokenId word")?)?;
    let maker_amt = word_u128(w.get(2).ok_or("no makerAmt word")?)? as f64 / USDC_DECIMALS;
    let taker_amt = word_u128(w.get(3).ok_or("no takerAmt word")?)? as f64 / USDC_DECIMALS;
    let fee = word_u128(w.get(4).ok_or("no fee word")?)? as f64 / USDC_DECIMALS;
    let (size, usd) = match side {
        Side::Buy => (taker_amt, maker_amt),
        Side::Sell => (maker_amt, taker_amt),
    };
    if size == 0.0 {
        return Err("zero-size fill".to_string());
    }
    let taker = word_address(&log_topic(l, 3)?)?;
    let role =
        if taker.eq_ignore_ascii_case(V2_EXCHANGE) { FillRole::Taker } else { FillRole::Maker };
    Ok(OrderFill {
        abi: OrderFillAbi::V2,
        order_hash: log_topic(l, 1)?,
        maker: word_address(&log_topic(l, 2)?)?,
        taker,
        token_id,
        side,
        size,
        price: usd / size,
        fee_usdc: fee,
        role,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: (log_u64(l, "blockTimestamp") as i64).saturating_mul(1000),
    })
}

/// V1 (8-param, legacy) `OrderFilled` on [`V1_EXCHANGE_STD`]/[`V1_EXCHANGE_NEG`]. Topics:
/// `[topic0, orderHash, maker, taker]`; data is `[makerAssetId, takerAssetId, makerAmt, takerAmt,
/// fee]`. `makerAssetId == 0` (the USDC collateral asset id) ⇒ BUY (maker pays USDC, token =
/// takerAssetId); `takerAssetId == 0` ⇒ SELL. Neither leg being the USDC asset id is not a
/// collateral fill (a token/token match) and is rejected, mirroring the Python decoder's `None`.
pub fn decode_order_fill_v1(l: &Value) -> Result<OrderFill, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_ORDER_FILLED_V1) {
        return Err(format!("not a V1 OrderFilled log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    let maker_asset = w.first().ok_or("no makerAssetId word")?;
    let taker_asset = w.get(1).ok_or("no takerAssetId word")?;
    let maker_amt = word_u128(w.get(2).ok_or("no makerAmt word")?)? as f64 / USDC_DECIMALS;
    let taker_amt = word_u128(w.get(3).ok_or("no takerAmt word")?)? as f64 / USDC_DECIMALS;
    let fee = word_u128(w.get(4).ok_or("no fee word")?)? as f64 / USDC_DECIMALS;
    let (token_id, side, size, usd) = if word_u128(maker_asset)? == 0 {
        (u256_word_to_decimal(taker_asset)?, Side::Buy, taker_amt, maker_amt)
    } else if word_u128(taker_asset)? == 0 {
        (u256_word_to_decimal(maker_asset)?, Side::Sell, maker_amt, taker_amt)
    } else {
        return Err("neither asset leg is USDC — not a collateral fill".to_string());
    };
    if size == 0.0 {
        return Err("zero-size fill".to_string());
    }
    let taker = word_address(&log_topic(l, 3)?)?;
    let role = if taker.eq_ignore_ascii_case(V1_EXCHANGE_STD)
        || taker.eq_ignore_ascii_case(V1_EXCHANGE_NEG)
    {
        FillRole::Taker
    } else {
        FillRole::Maker
    };
    Ok(OrderFill {
        abi: OrderFillAbi::V1,
        order_hash: log_topic(l, 1)?,
        maker: word_address(&log_topic(l, 2)?)?,
        taker,
        token_id,
        side,
        size,
        price: usd / size,
        fee_usdc: fee,
        role,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: (log_u64(l, "blockTimestamp") as i64).saturating_mul(1000),
    })
}

/// Which position-accounting event a [`PositionEvent`] decoded — CTF `PositionSplit` mints a full
/// outcome-token set against collateral; `PositionsMerge` burns one back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEventKind {
    Split,
    Merge,
}

/// One decoded CTF `PositionSplit`/`PositionsMerge` log — the position-accounting twin of a
/// redemption. Topics: `[topic0, stakeholder, parentCollectionId, conditionId]`; data head is
/// `[collateralToken, offset(partition), amount, ...]` — offset is ALWAYS `0x60` (three head
/// words), the same shape [`decode_ctf_redemption`] documents. `amount` is the full-SET mint/burn
/// total, not a per-outcome value.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionEvent {
    pub kind: PositionEventKind,
    pub stakeholder: String,
    pub condition_id: String,
    pub amount: f64,
    pub tx_hash: String,
    pub block: u64,
}

fn decode_ctf_position_event(
    l: &Value,
    topic0: &str,
    kind: PositionEventKind,
) -> Result<PositionEvent, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(topic0) {
        return Err(format!("not a CTF {kind:?} log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(PositionEvent {
        kind,
        stakeholder: word_address(&log_topic(l, 1)?)?,
        condition_id: log_topic(l, 3)?,
        amount: word_u128(w.get(2).ok_or("no amount word")?)? as f64 / USDC_DECIMALS,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// CTF `PositionSplit` → [`PositionEvent`].
pub fn decode_ctf_position_split(l: &Value) -> Result<PositionEvent, String> {
    decode_ctf_position_event(l, TOPIC_CTF_POSITION_SPLIT, PositionEventKind::Split)
}

/// CTF `PositionsMerge` → [`PositionEvent`].
pub fn decode_ctf_position_merge(l: &Value) -> Result<PositionEvent, String> {
    decode_ctf_position_event(l, TOPIC_CTF_POSITION_MERGE, PositionEventKind::Merge)
}

/// One decoded NegRiskAdapter `PositionsConverted` log — converts YES tokens across a neg-risk
/// market's outcome set. Topics: `[topic0, stakeholder, marketId, indexSet]`; data is a single
/// word `[amount]`. `index_set` is a BITMAP over the market's outcome slots, NOT a per-slot amount.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionsConverted {
    pub stakeholder: String,
    pub market_id: String,
    /// Bitmap over outcome slots — NOT per-slot amounts (see `polymarket-negrisk-set-facts`).
    pub index_set: u128,
    pub amount: f64,
    pub tx_hash: String,
    pub block: u64,
}

/// NegRiskAdapter `PositionsConverted` → [`PositionsConverted`].
pub fn decode_positions_converted(l: &Value) -> Result<PositionsConverted, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_NEG_RISK_POSITIONS_CONVERTED) {
        return Err(format!("not a NegRiskAdapter PositionsConverted log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(PositionsConverted {
        stakeholder: word_address(&log_topic(l, 1)?)?,
        market_id: log_topic(l, 2)?,
        index_set: word_u128(&log_topic(l, 3)?)?,
        amount: word_u128(w.first().ok_or("no amount word")?)? as f64 / USDC_DECIMALS,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// One decoded USDC.e ERC-20 `Transfer` — the funding/withdrawal leg no CLOB endpoint reports
/// (deposits, a redemption payout landing back in the funder's wallet, on-chain withdrawals).
/// Standard ERC-20 shape: topics `[topic0, from, to]`, data `[value]` (6-dp, [`USDC_DECIMALS`]).
#[derive(Debug, Clone, PartialEq)]
pub struct UsdcTransfer {
    pub from: String,
    pub to: String,
    pub value_usdc: f64,
    pub tx_hash: String,
    pub block: u64,
}

/// USDC.e `Transfer` → [`UsdcTransfer`].
pub fn decode_usdc_transfer(l: &Value) -> Result<UsdcTransfer, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_USDC_TRANSFER) {
        return Err(format!("not a USDC Transfer log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(UsdcTransfer {
        from: word_address(&log_topic(l, 1)?)?,
        to: word_address(&log_topic(l, 2)?)?,
        value_usdc: word_u128(w.first().ok_or("no value word")?)? as f64 / USDC_DECIMALS,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}
