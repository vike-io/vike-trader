//! **The OKX instrument CLASSES this adapter signs, read from the instrument's own id** (decision 0232).
//!
//! The adapter used to sign SWAP perpetuals only. An account now also trades a SPOT pair (`BTC-USDT`) and a
//! dated USDT-linear FUTURE (`QTUM-USDT-261225`) on the same engine: the class changes five things and nothing
//! else, so one pure module states them and every call site asks it instead of hard-coding `"SWAP"`:
//!
//! | what | SWAP / FUTURES | SPOT |
//! |---|---|---|
//! | `instType` of every read (`orders-history`, `fills-history`, `positions`, `public/instruments`) | `SWAP` / `FUTURES` | `SPOT` (the venue answers `51015` to a mismatch, MEASURED) |
//! | `tdMode` | `cross` / `isolated` per order | `cross` at account level 2, 3 and 4, `cash` at level 1 ([`SpotMode`]) |
//! | `sz` | contracts (`ctVal` base each) | the BASE coin; a MARKET order states `tgtCcy=base_ccy`, or `sz` is the QUOTE amount |
//! | the fee of a BUY | the settle currency | the BASE coin (netted out of the fill: [`net_spot_buy_fee`]) |
//! | positions | `positions?instType=` | none (the venue refuses `instType=SPOT`); a holding is the balance, not reported |
//!
//! A class is told by the SHAPE of the `instId` (`BASE-QUOTE`, `BASE-QUOTE-SWAP`, `BASE-QUOTE-YYMMDD`), and
//! the instrument row's own `instType` is held against it before anything is signed
//! (`crate::exec`'s `foreign_rest`), so a shape that lies refuses instead of signing in the wrong unit. An id
//! of no known shape reads as a SWAP, as every id did before this module existed: the mounted instrument is
//! always `...-SWAP`, so its requests keep their bytes.

use vike_model::events::FillEvent;

/// How close to its delivery a dated future still takes an OPENING order (and is admitted): the engine
/// books no delivery settlement, so a position the venue settles at the expiry stays in the engine's book
/// until an operator confirms the drift. A reduce-only order is taken until the expiry itself.
pub const EXPIRY_GUARD_MS: i64 = 24 * 60 * 60 * 1000;

/// OKX settles and delists a dated future at 08:00 UTC of its expiry date (`expTime` of every future the
/// demo listed on 2026-10-30, `1798185600000` = 2026-12-25 08:00:00 UTC).
const DELIVERY_HOUR_MS: i64 = 8 * 60 * 60 * 1000;

/// An instrument class the adapter signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstClass {
    /// A perpetual swap, `BASE-QUOTE-SWAP`.
    Swap,
    /// A dated future, `BASE-QUOTE-YYMMDD`.
    Futures,
    /// A spot pair, `BASE-QUOTE`.
    Spot,
}

impl InstClass {
    /// The class `inst_id` (upper case) is spelled as, or `None` for a spelling that is none of the three (an
    /// option `BASE-QUOTE-YYMMDD-STRIKE-C`, anything with a character an `instId` does not have).
    #[must_use]
    pub fn of_inst_id(inst_id: &str) -> Option<InstClass> {
        let parts: Vec<&str> = inst_id.split('-').collect();
        if !parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        {
            return None;
        }
        match parts.as_slice() {
            [_, _] => Some(InstClass::Spot),
            [_, _, tail] if tail.eq_ignore_ascii_case("SWAP") => Some(InstClass::Swap),
            [_, _, tail] if is_yymmdd(tail) => Some(InstClass::Futures),
            _ => None,
        }
    }

    /// [`Self::of_inst_id`], with an id of no known shape read as a SWAP (what every id was).
    #[must_use]
    pub fn of_inst_id_or_swap(inst_id: &str) -> InstClass {
        Self::of_inst_id(inst_id).unwrap_or(InstClass::Swap)
    }

    /// The venue's `instType` word for the class: the filter of every read that takes one.
    #[must_use]
    pub const fn inst_type(self) -> &'static str {
        match self {
            InstClass::Swap => "SWAP",
            InstClass::Futures => "FUTURES",
            InstClass::Spot => "SPOT",
        }
    }
}

/// Six digits that read as `YYMMDD` (a month 1 to 12, a day 1 to 31).
fn is_yymmdd(tail: &str) -> bool {
    let b = tail.as_bytes();
    b.len() == 6
        && b.iter().all(u8::is_ascii_digit)
        && matches!(
            &tail[2..4],
            "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12"
        )
        && (1..=31).contains(&tail[4..6].parse::<u32>().unwrap_or(0))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The delivery instant of a dated future from its id (08:00 UTC of the `YYMMDD` it ends in), in epoch
/// milliseconds; `None` for any other class. The id carries it, so the order path needs no row to guard it.
#[must_use]
pub fn future_expiry_ms(inst_id: &str) -> Option<i64> {
    if InstClass::of_inst_id(inst_id) != Some(InstClass::Futures) {
        return None;
    }
    let tail = inst_id.rsplit('-').next()?;
    let (yy, mm, dd) = (
        tail[0..2].parse::<i64>().ok()?,
        tail[2..4].parse::<i64>().ok()?,
        tail[4..6].parse::<i64>().ok()?,
    );
    Some(days_from_civil(2000 + yy, mm, dd) * 86_400_000 + DELIVERY_HOUR_MS)
}

/// Why an order on the dated future `inst_id` must not be sent at `now_ms`, or `None`. An OPENING order is
/// refused inside [`EXPIRY_GUARD_MS`] of the delivery; any order is refused once the delivery has passed (the
/// instrument no longer exists); a REDUCE-ONLY order is taken until then. Another class is never refused.
#[must_use]
pub fn expiry_refusal(inst_id: &str, now_ms: i64, reduce_only: bool) -> Option<String> {
    let expiry = future_expiry_ms(inst_id)?;
    if now_ms >= expiry {
        return Some(format!(
            "okx: {inst_id} was delivered at its expiry (08:00 UTC of the date in its name) and no \
             longer trades; the order was not sent"
        ));
    }
    if !reduce_only && now_ms >= expiry - EXPIRY_GUARD_MS {
        let hours = (expiry - now_ms) / 3_600_000;
        return Some(format!(
            "okx: {inst_id} is delivered in {hours} h and takes no OPENING order inside the last 24 h: \
             the venue settles a position at the delivery and the engine books no settlement, so it \
             would hold a position the venue closed. A reduce-only order is still taken; open the next \
             expiry instead"
        ));
    }
    None
}

/// The `tdMode` a SPOT order is written with. The account's LEVEL decides, not the order: at level 1 (spot
/// mode) the venue takes `cash`; at level 2, 3 and 4 (the margin modes) it takes `cross` and answers `cash`
/// "Parameter tdMode error" (`51000`), both MEASURED on the demo account (level 3) on 2026-10-30 for the
/// order endpoint and the algo endpoint alike. The level is not known where the order is written (a live
/// account is never read for its mode), so the order is sent as [`SpotMode::Cross`] first and, on exactly
/// that refusal, once more as [`SpotMode::Cash`] (`crate::perp`'s `place_spot`): a refused order rests
/// nowhere, so the second try cannot double an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotMode {
    Cross,
    Cash,
}

impl SpotMode {
    /// The wire word.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            SpotMode::Cross => "cross",
            SpotMode::Cash => "cash",
        }
    }

    /// The other mode.
    #[must_use]
    pub const fn other(self) -> SpotMode {
        match self {
            SpotMode::Cross => SpotMode::Cash,
            SpotMode::Cash => SpotMode::Cross,
        }
    }
}

/// Whether a venue refusal (`code`, `msg`) is the `tdMode` refusal of an account whose level does not take the
/// mode just sent: code `51000` (a parameter error) whose message names `tdMode`.
#[must_use]
pub fn is_td_mode_refusal(code: i64, msg: &str) -> bool {
    code == 51000 && msg.to_ascii_lowercase().contains("tdmode")
}

/// **A spot BUY's fill, net of the fee the venue took out of the coin it bought** (MEASURED 2026-10-30: a
/// market buy of 0.00002 BTC filled 0.00002 and charged `fee -0.00000002`, `feeCcy BTC`; the account's BTC rose
/// by the quantity less the fee). The engine books a commission as units of the QUOTE currency and the
/// quantity as what the account holds, so a base-coin fee is restated: the quantity loses the fee, the
/// commission becomes `fee x fill price` in the quote asset. Without it the engine's long exceeds the wallet's
/// coin by the sum of the fees and a sale of the whole position asks for coin the wallet lacks.
///
/// Touches only a SPOT symbol's BUY fill that carries a positive commission in its base asset and that is
/// smaller than the fill; a SELL (its fee is already the quote), a rebate, a fee in a third asset and every
/// derivative keep what they carry. Applied once, on the stream, the history replay and the reconcile fill
/// read alike, so a fill replayed after a gap is the fill the stream folded. A repeat on an already-netted
/// fill is a no-op (the commission is no longer in the base asset).
pub fn net_spot_buy_fee(fill: &mut FillEvent) {
    if let Some((qty, commission, asset)) = spot_buy_net(
        fill.symbol.as_str(),
        fill.side,
        fill.last_qty,
        fill.last_px,
        fill.commission,
        fill.commission_asset.as_str(),
    ) {
        fill.last_qty = qty;
        fill.commission = commission;
        fill.commission_asset = asset.as_str().into();
    }
}

/// [`net_spot_buy_fee`]'s arithmetic over the bare numbers (the reconcile read's `FillReport` is not a
/// `FillEvent`): the restated `(quantity, commission, commission asset)` of a spot BUY paid in its base coin,
/// or `None` when the fill is not one.
#[must_use]
pub fn spot_buy_net(
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
    commission: f64,
    commission_asset: &str,
) -> Option<(f64, f64, String)> {
    if side <= 0 || InstClass::of_inst_id(symbol) != Some(InstClass::Spot) {
        return None;
    }
    let mut halves = symbol.split('-');
    let (base, quote) = (halves.next().unwrap_or(""), halves.next().unwrap_or(""));
    if base.is_empty()
        || quote.is_empty()
        || !commission_asset.eq_ignore_ascii_case(base)
        || !(commission.is_finite() && commission > 0.0 && commission < qty)
    {
        return None;
    }
    Some((qty - commission, commission * px, quote.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::{LiquiditySide, PositionSide, TradeId};

    #[test]
    fn a_class_is_read_from_the_shape_of_the_inst_id() {
        for (id, class) in [
            ("BTC-USDT", Some(InstClass::Spot)),
            ("BTC-USDT-SWAP", Some(InstClass::Swap)),
            ("btc-usdt-swap", Some(InstClass::Swap)),
            ("QTUM-USDT-261225", Some(InstClass::Futures)),
            ("BTC-USD_UM-SWAP", Some(InstClass::Swap)),
            ("SOL-USD_UM-261127", Some(InstClass::Futures)),
            ("BTC-USD-261106-76000-C", None),
            ("BTC-USDT-26-1225", None),
            ("BTC-USDT-261325", None),
            ("BTC-USDT-261200", None),
            ("BTCUSDT", None),
            ("BTC--SWAP", None),
            ("BTC-USDT-SWAP-X", None),
            ("", None),
        ] {
            assert_eq!(InstClass::of_inst_id(id), class, "{id:?}");
        }
        assert_eq!(InstClass::of_inst_id_or_swap("weird"), InstClass::Swap);
        assert_eq!(InstClass::Spot.inst_type(), "SPOT");
        assert_eq!(InstClass::Futures.inst_type(), "FUTURES");
        assert_eq!(InstClass::Swap.inst_type(), "SWAP");
    }

    /// The venue's own `expTime` of the three dated futures the demo listed on 2026-10-30.
    #[test]
    fn the_delivery_instant_is_the_venues_exp_time() {
        for (id, exp_time) in [
            ("QTUM-USDT-261225", 1_798_185_600_000_i64),
            ("GRASS-USDT-261106", 1_793_952_000_000),
            ("XRP-USD-270326", 1_806_048_000_000),
            ("BTC-USD-271231", 1_830_240_000_000),
        ] {
            assert_eq!(future_expiry_ms(id), Some(exp_time), "{id}");
        }
        assert_eq!(future_expiry_ms("BTC-USDT-SWAP"), None);
        assert_eq!(future_expiry_ms("BTC-USDT"), None);
    }

    #[test]
    fn an_opening_order_is_refused_in_the_last_day_and_everything_after_the_delivery() {
        let id = "QTUM-USDT-261225";
        let expiry = future_expiry_ms(id).unwrap();
        // far from the delivery: nothing is refused
        assert_eq!(expiry_refusal(id, expiry - 30 * 86_400_000, false), None);
        // just outside the guard
        assert_eq!(expiry_refusal(id, expiry - EXPIRY_GUARD_MS - 1, false), None);
        // inside the guard: an opening order is refused in words, a reduce-only order is taken
        let why = expiry_refusal(id, expiry - 3 * 3_600_000, false).expect("refused");
        assert!(why.contains("OPENING") && why.contains("3 h"), "{why}");
        assert_eq!(expiry_refusal(id, expiry - 3 * 3_600_000, true), None);
        // at and after the delivery everything is refused
        for reduce_only in [false, true] {
            let why = expiry_refusal(id, expiry, reduce_only).expect("delivered");
            assert!(why.contains("delivered"), "{why}");
        }
        // another class is never refused
        assert_eq!(expiry_refusal("BTC-USDT-SWAP", expiry + 1, false), None);
        assert_eq!(expiry_refusal("BTC-USDT", expiry + 1, false), None);
    }

    #[test]
    fn the_td_mode_refusal_is_exactly_the_measured_one() {
        assert!(is_td_mode_refusal(51000, "Parameter tdMode error "));
        assert!(!is_td_mode_refusal(51000, "Parameter sz error"));
        assert!(!is_td_mode_refusal(51020, "tdMode"));
        assert_eq!(SpotMode::Cross.other(), SpotMode::Cash);
        assert_eq!(SpotMode::Cash.wire(), "cash");
    }

    fn fill(symbol: &str, side: i32, qty: f64, px: f64, fee: f64, ccy: &str) -> FillEvent {
        FillEvent {
            trade_id: TradeId::new("1").unwrap(),
            client_order_id: "c".into(),
            venue: "okx".into(),
            symbol: symbol.into(),
            side,
            last_qty: qty,
            last_px: px,
            commission: fee,
            commission_asset: ccy.into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 0,
            mark_price: None,
            position_side: PositionSide::Both,
        }
    }

    /// The MEASURED fill: 0.00002 BTC bought at 76927.2, `fee -0.00000002 BTC`, so a commission of +2e-8 BTC.
    #[test]
    fn a_spot_buy_paid_in_the_base_coin_is_restated_net() {
        let mut f = fill("BTC-USDT", 1, 0.00002, 76927.2, 0.00000002, "BTC");
        net_spot_buy_fee(&mut f);
        assert!((f.last_qty - 0.00001998).abs() < 1e-15, "{}", f.last_qty);
        assert!((f.commission - 0.00000002 * 76927.2).abs() < 1e-12, "{}", f.commission);
        assert_eq!(f.commission_asset.as_str(), "USDT");
        // applied again it changes nothing: the commission is no longer in the base asset
        let again = f.clone();
        net_spot_buy_fee(&mut f);
        assert_eq!(f, again);
    }

    #[test]
    fn nothing_else_is_netted() {
        // a SELL pays in the quote already (MEASURED: fee -0.0065 USDT)
        let sell = fill("BTC-USDT", -1, 0.00008492, 76903.3, 0.006530628236, "USDT");
        // a rebate, a fee in a third asset, a fee not smaller than the fill, a non-finite one, a derivative
        let cases = [
            sell,
            fill("BTC-USDT", 1, 0.00002, 76927.2, -0.00000002, "BTC"),
            fill("BTC-USDT", 1, 0.00002, 76927.2, 0.5, "OKB"),
            fill("BTC-USDT", 1, 0.00002, 76927.2, 0.00002, "BTC"),
            fill("BTC-USDT", 1, 0.00002, 76927.2, f64::NAN, "BTC"),
            fill("BTC-USDT-SWAP", 1, 0.01, 76927.2, 0.0001, "BTC"),
            fill("QTUM-USDT-261225", 1, 10.0, 0.77, 0.003888, "USDT"),
        ];
        for original in cases {
            let mut f = original.clone();
            net_spot_buy_fee(&mut f);
            assert_eq!(format!("{f:?}"), format!("{original:?}"), "{original:?}");
        }
    }
}
