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
//! A class is told by the SHAPE of the `instId` (`BASE-QUOTE`, `BASE-QUOTE-SWAP`, `BASE-QUOTE-YYMMDD`; the ONE reading
//! of it, OKX-2: the door reads user names since decision 0290), and the instrument row's own `instType` is held
//! against it before anything is signed (`crate::execution`'s `foreign_rest`), so a shape that lies refuses instead of
//! signing in the wrong unit. An id of no known shape is refused in words wherever a class must be named
//! ([`InstrumentType::of_inst_id_or_refusal`]), never read as a SWAP (it was until 2026-11-16, audit flag OKX-1).
//!
//! **The instId lives only inside this bridge** (decision 0290). The engine, the journal, the fills and every door
//! speak the USER name: the spot pair `BTCUSDT` (`BTC-USDT`), the perpetual `BTCUSDT.P` (`BTC-USDT-SWAP`), the dated
//! future `BTCUSDT_261225` (`BTC-USDT-261225`). [`inst_id_of`] reads a user name's instId off its KEPT row (the
//! datahub's `<project>/instruments/okx.json`, `vike_catalog::kept_raw_of_user_name`; no list, no row or two rows is a
//! refusal in words), and [`user_name_of`] is the way back for every row the venue sends, applied where events and
//! reports leave the bridge ([`speak_user_names`]); the code between speaks instIds.

use vike_bridge_core::event_map::{SpotFeeArm, SpotFeeArms, spot_base_fee_net};
use vike_model::events::{Event, FillEvent};
use vike_utility::calendar::days_from_civil;
use vike_venues::venue_delivery_guard::{
    DeliveryGuardVerdict, delivery_guard_for, delivery_verdict,
};

use crate::refusals::OkxInstrumentRefusal;

const VENUE: &str = "okx";

/// **The instId of a USER name** (decision 0290): its KEPT row's (`BTC-USDT-SWAP` for `BTCUSDT.P`). What every order,
/// cancel, status query, account read, socket and history read of the name signs or asks.
///
/// # Errors
/// No okx list is kept in this process, none of its rows is `user`, or two are (okx's inverse and USD-margined dated
/// futures of one day, decision 0290's O-9): each in words naming `user`.
pub fn inst_id_of(user: &str) -> Result<String, OkxInstrumentRefusal> {
    vike_catalog::kept_raw_of_user_name(VENUE, user)
        .map_err(|why| OkxInstrumentRefusal::NoKeptRow { why })
}

/// **The USER name a venue row's `inst_id` is booked under** (a fill, a liquidation, a funding bill, a reconcile
/// report): its kept row's user name, or the venue's own `inst_id` said at WARN when no kept row is named so (an
/// instrument the operator traded by hand that the list does not carry yet: no name this process signed).
#[must_use]
pub fn user_name_of(inst_id: &str) -> String {
    vike_catalog::kept_user_name_of_raw(VENUE, inst_id).unwrap_or_else(|| {
        tracing::warn!(
            venue = VENUE,
            inst_id,
            "a venue row names an instrument no kept row names (the datahub's <project>/instruments/okx.json); it is \
             booked under the venue's own name"
        );
        inst_id.to_string()
    })
}

/// **`events` as they leave the bridge**: every symbol they carry (a fill's, a liquidation's, a funding settlement's)
/// respelled from the instId to its USER name ([`user_name_of`]).
#[must_use]
pub fn speak_user_names(mut events: Vec<Event>) -> Vec<Event> {
    vike_bridge_core::event_map::respell_symbols(&mut events, user_name_of);
    events
}

/// The instIds [`kept_list_for_tests`] holds: every one an okx test names, so each test of a path that reads a user
/// name holds the SAME list and none replaces another's (the tests of one binary share the process).
#[cfg(any(test, feature = "test-support"))]
const TEST_INST_IDS: &[&str] = &[
    "BTC-USDT-SWAP",
    "ETH-USDT-SWAP",
    "DOGE-USDT-SWAP",
    "XRP-USDT-SWAP",
    "SOL-USDT-SWAP",
    "X-USDT-SWAP",
    "GONE-USDT-SWAP",
    "BUSY-USDT-SWAP",
    "MULT-USDT-SWAP",
    "NOCAP-USDT-SWAP",
    "EMPTY-USDT-SWAP",
    "NULL-USDT-SWAP",
    "NOLEVER-USDT-SWAP",
    "JUNK-USDT-SWAP",
    "USD-INVERSE-SWAP",
    "BTC-USD-SWAP",
    "ETH-USD-SWAP",
    "BTC-USDT",
    "BTC-USD",
    "ETH-USD",
    "SOL-USD",
    "XAU-USD",
    "QTUM-USDT-261225",
    "QTUM-USDT-991231",
    "QTUM-USDT-200101",
    "GRASS-USDT-261106",
    "BTC-USDT-261225",
    "BTC-USD-261030",
    "BTC-USD-991231",
];

/// **Test support: hold okx's kept list in this process** (`vike_catalog::hold_kept_list`): a row of every instId the
/// tests name ([`TEST_INST_IDS`]), each of the class its id spells: a spot pair, a SWAP perpetual (coin-margined when
/// quoted in `USD`, as `BTC-USD-SWAP`) or a dated future delivered on the day it names. What a test that holds no
/// datahub hands every reader of a user name (`BTCUSDT.P` is `BTC-USDT-SWAP`); a second call changes nothing.
#[cfg(any(test, feature = "test-support"))]
pub fn kept_list_for_tests() {
    use vike_model::{Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily};
    use vike_model::{ContractPerp, Instrument, Margining};
    let rows = TEST_INST_IDS
        .iter()
        .map(|id| {
            let id = id.to_ascii_uppercase();
            let mut parts = id.split('-');
            let (base, quote) =
                (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
            let margining = if quote == "USD" { Margining::Inverse } else { Margining::Linear };
            let settle = if quote == "USD" { base } else { quote }.to_string();
            let contract = match InstrumentType::of_inst_id(&id) {
                Some(InstrumentType::Spot) => Contract::CryptoSpot,
                Some(InstrumentType::Futures) => Contract::Future(ContractFuture {
                    margining,
                    settle,
                    value_currency: None,
                    expiry_ms: future_expiry_ms(&id).unwrap_or_default(),
                    tenor: None,
                    delivery: ContractFutureDelivery::Cash,
                    family: ContractFutureFamily::Crypto,
                }),
                _ => Contract::Perp(ContractPerp {
                    margining,
                    settle,
                    value_currency: None,
                    funding_interval_ms: None,
                    settling_ms: None,
                }),
            };
            Instrument::new(VENUE, &id, base, quote, contract)
        })
        .collect();
    vike_catalog::hold_kept_list(VENUE, rows);
}

/// An instrument class the adapter signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstrumentType {
    /// A perpetual swap, `BASE-QUOTE-SWAP`.
    Swap,
    /// A dated future, `BASE-QUOTE-YYMMDD`.
    Futures,
    /// A spot pair, `BASE-QUOTE`.
    Spot,
}

impl InstrumentType {
    /// The class `inst_id` (any case, trimmed) is spelled as, or `None` for a spelling that is none of the three: the
    /// ONE instId grammar (OKX-2). Letters and digits of at most 16 between hyphens, the venue's own underscore in a
    /// three-part id's quote slot alone (`USD_UM`), a spot pair's quote never `SWAP`, at most 48 characters, and a
    /// dated tail that is a date ([`is_delivery_date`]). An option (`BASE-QUOTE-YYMMDD-STRIKE-C`) and anything else
    /// are `None`.
    #[must_use]
    pub fn of_inst_id(inst_id: &str) -> Option<InstrumentType> {
        let upper = inst_id.trim().to_ascii_uppercase();
        let word = |p: &str| {
            !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric())
        };
        // the quote slot of a three-part id may carry the venue's own underscore (`USD_UM`)
        let quote = |p: &str| word(&p.replace('_', "")) && !p.starts_with('_') && !p.ends_with('_');
        if upper.len() > 48 {
            return None;
        }
        match upper.split('-').collect::<Vec<_>>().as_slice() {
            [base, quote_coin] if word(base) && word(quote_coin) && *quote_coin != "SWAP" => {
                Some(InstrumentType::Spot)
            }
            [base, quote_coin, "SWAP"] if word(base) && quote(quote_coin) => {
                Some(InstrumentType::Swap)
            }
            [base, quote_coin, tail]
                if word(base) && quote(quote_coin) && is_delivery_date(tail) =>
            {
                Some(InstrumentType::Futures)
            }
            _ => None,
        }
    }

    /// [`Self::of_inst_id`], or the refusal in words: an id of no known form is NEVER read as a SWAP (a request
    /// stamped `instType=SWAP` for it would read or sign another class than the id is), so a caller that must name a
    /// class says why it cannot and sends nothing.
    ///
    /// # Errors
    /// Why `inst_id` is no instrument id this adapter can class, naming it.
    pub fn of_inst_id_or_refusal(inst_id: &str) -> Result<InstrumentType, OkxInstrumentRefusal> {
        Self::of_inst_id(inst_id)
            .ok_or_else(|| OkxInstrumentRefusal::UnknownForm { inst_id: inst_id.to_string() })
    }

    /// The venue's `instType` word for the class: the filter of every read that takes one.
    #[must_use]
    pub const fn inst_type(self) -> &'static str {
        match self {
            InstrumentType::Swap => "SWAP",
            InstrumentType::Futures => "FUTURES",
            InstrumentType::Spot => "SPOT",
        }
    }
}

/// **The delivery date of a dated future's id** (`BTC-USDT-261225`'s `261225`): six digits that read as `YYMMDD`, a
/// month 1 to 12 and a day 1 to 31.
fn is_delivery_date(tail: &str) -> bool {
    let b = tail.as_bytes();
    b.len() == 6
        && b.iter().all(u8::is_ascii_digit)
        && (1..=12).contains(&tail[2..4].parse::<u32>().unwrap_or(0))
        && (1..=31).contains(&tail[4..6].parse::<u32>().unwrap_or(0))
}

/// The delivery instant of a dated future from its id (the `YYMMDD` it ends in, at okx's delivery time of day,
/// 08:00 UTC: `vike_venues::venue_delivery_guard`'s row, equal to the `expTime` of every future the demo listed on
/// 2026-10-30), in epoch milliseconds; `None` for any other class. What a test's kept row is built with
/// (`kept_list_for_tests`); no guard reads it (the guard reads the USER name: [`expiry_refusal`]).
#[must_use]
pub fn future_expiry_ms(inst_id: &str) -> Option<i64> {
    if InstrumentType::of_inst_id(inst_id) != Some(InstrumentType::Futures) {
        return None;
    }
    let tail = inst_id.rsplit('-').next()?;
    let (yy, mm, drawdown) = (
        tail[0..2].parse::<i64>().ok()?,
        tail[2..4].parse::<u32>().ok()?,
        tail[4..6].parse::<u32>().ok()?,
    );
    Some(
        days_from_civil(2000 + yy, mm, drawdown) * vike_model::MS_PER_DAY
            + delivery_guard_for(VENUE).delivery_time_of_day_ms,
    )
}

/// Why an order on `symbol` (the engine's USER name: a dated future is `BTCUSDT_261225`) must not be sent at
/// `now_ms`, or `None`: `vike_venues::venue_delivery_guard`'s `delivery_verdict` (okx's rule, every venue's since the
/// owner's ruling of 2026-11-22) in okx's words. An OPENING order is refused inside the last day of the delivery the
/// name states; any order is refused once it has passed (the instrument no longer exists); a REDUCE-ONLY order is
/// taken until then. A name that states no delivery day (a SWAP, a spot pair) is never refused.
#[must_use]
pub fn expiry_refusal(symbol: &str, now_ms: i64, reduce_only: bool) -> Option<String> {
    match delivery_verdict(VENUE, symbol, now_ms, reduce_only) {
        DeliveryGuardVerdict::Taken => None,
        DeliveryGuardVerdict::Delivered => Some(format!(
            "okx: {symbol} was delivered at its expiry (08:00 UTC of the date in its name) and no \
             longer trades; the order was not sent"
        )),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms } => {
            let hours = left_ms / vike_model::MS_PER_HOUR;
            Some(format!(
                "okx: {symbol} is delivered in {hours} h and takes no OPENING order inside the last 24 h: \
                 the venue settles a position at the delivery and the engine books no settlement, so it \
                 would hold a position the venue closed. A reduce-only order is still taken; open the next \
                 expiry instead"
            ))
        }
    }
}

/// The `tdMode` a SPOT order is written with. The account's LEVEL decides, not the order: at level 1 (spot
/// mode) the venue takes `cash`; at level 2, 3 and 4 (the margin modes) it takes `cross` and answers `cash`
/// "Parameter tdMode error" (`51000`), both MEASURED on the demo account (level 3) on 2026-10-30 for the
/// order endpoint and the algo endpoint alike. The level is not known where the order is written (a live
/// account is never read for its mode), so the order is sent as [`SpotMode::Cross`] first and, on exactly
/// that refusal, once more as [`SpotMode::Cash`] (`crate::execution::rest`'s `place_spot`): a refused order rests
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
        fill.last_price,
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
    if InstrumentType::of_inst_id(symbol) != Some(InstrumentType::Spot) {
        return None;
    }
    let mut halves = symbol.split('-');
    let (base, quote) = (halves.next().unwrap_or(""), halves.next().unwrap_or(""));
    if base.is_empty() || quote.is_empty() || !commission_asset.eq_ignore_ascii_case(base) {
        return None;
    }
    let (net, commission) = spot_base_fee_net(side, qty, px, commission, SPOT_FEE_ARMS)?;
    Some((net, commission, quote.to_string()))
}

/// A SELL pays in the quote (MEASURED: `fee -0.0065`, `feeCcy USDT`), so this venue's SELL arm has never been reached,
/// and no spot rebate is in the tree. Were either stated in the base coin the wallet would move by it and this leaves
/// it out: the quantity short of the wallet and the fee in the wrong unit. Kept until a ruling or a measured row.
const SPOT_FEE_ARMS: SpotFeeArms =
    SpotFeeArms { sell: SpotFeeArm::AsStated, rebate: SpotFeeArm::AsStated };

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::{LiquiditySide, PositionSide, TradeId};

    #[test]
    fn a_class_is_read_from_the_shape_of_the_inst_id() {
        for (id, class) in [
            ("BTC-USDT", Some(InstrumentType::Spot)),
            ("BTC-USDT-SWAP", Some(InstrumentType::Swap)),
            ("btc-usdt-swap", Some(InstrumentType::Swap)),
            ("QTUM-USDT-261225", Some(InstrumentType::Futures)),
            ("BTC-USD_UM-SWAP", Some(InstrumentType::Swap)),
            ("SOL-USD_UM-261127", Some(InstrumentType::Futures)),
            ("BTC-USD-261106-76000-C", None),
            ("BTC-USDT-26-1225", None),
            ("BTC-USDT-261325", None),
            ("BTC-USDT-261200", None),
            ("BTCUSDT", None),
            ("BTC--SWAP", None),
            ("BTC-USDT-SWAP-X", None),
            ("", None),
        ] {
            assert_eq!(InstrumentType::of_inst_id(id), class, "{id:?}");
        }
        assert_eq!(InstrumentType::Spot.inst_type(), "SPOT");
        assert_eq!(InstrumentType::Futures.inst_type(), "FUTURES");
        assert_eq!(InstrumentType::Swap.inst_type(), "SWAP");
    }

    /// **OKX-2: ONE instId grammar**, the bridge's own since the door reads user names (decision 0290): the spellings
    /// it once classed loosely (an underscore outside the quote slot, a pair whose quote is `SWAP`, an over-long word)
    /// are no class, and the venue's `USD_UM` quote slot is one.
    #[test]
    fn the_bridge_classes_an_id_by_its_one_grammar() {
        for id in
            ["ETH_USDT-SWAP", "BTC-SWAP", "HY_PE-USDT", "HYPE-USDT-SW_AP", "ABCDEFGHIJKLMNOPQ-USDT"]
        {
            assert_eq!(InstrumentType::of_inst_id(id), None, "{id}: no class");
        }
        for (id, class) in [
            ("eth-usdt", InstrumentType::Spot),
            ("NFLX-USD_UM_XPERP-310912", InstrumentType::Futures),
            ("HYPE-USD_UM-SWAP", InstrumentType::Swap),
        ] {
            assert_eq!(InstrumentType::of_inst_id(id), Some(class), "{id}");
        }
    }

    /// **OKX-1: an id of no known form is refused in words, never read as a SWAP.** Every caller that must name a
    /// class (`instType` of a history read, an order body, a stop, the leverage reads, the row fetch, the fee read)
    /// asks [`InstrumentType::of_inst_id_or_refusal`].
    #[test]
    fn an_id_of_no_known_form_is_refused_never_read_as_a_swap() {
        for id in ["weird", "BTCUSDT.P", "BTC-USD-261106-76000-C", "ETH-USDT-261399"] {
            let why = InstrumentType::of_inst_id_or_refusal(id).expect_err("no class guessed");
            assert_eq!(why, OkxInstrumentRefusal::UnknownForm { inst_id: id.to_string() });
        }
        assert_eq!(
            InstrumentType::of_inst_id_or_refusal("btc-usdt-swap"),
            Ok(InstrumentType::Swap)
        );
    }

    /// The shared spot row (`vike_venues::spot_pair_row`: what the fee lane, the wallet-cash reading and
    /// `vike_catalog::names_spot_pair` take as a parameter) spells a USER name (decision 0290), and it agrees with
    /// this bridge's own class of the instId the user name's KEPT row signs ([`InstrumentType::of_inst_id`]), over
    /// rows of the three classes it signs.
    #[test]
    fn the_shared_spot_row_is_this_bridges_spelling() {
        use vike_model::AssetClass;
        let spot = vike_venues::spot_pair_row("okx").expect("okx states its spot spelling");
        let perp = vike_model::Contract::Perp(vike_model::ContractPerp {
            margining: vike_model::Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        let future = vike_model::Contract::Future(vike_model::ContractFuture {
            margining: vike_model::Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
            expiry_ms: future_expiry_ms("QTUM-USDT-261225").expect("dated"),
            tenor: None,
            delivery: vike_model::ContractFutureDelivery::Cash,
            family: vike_model::ContractFutureFamily::Crypto,
        });
        let rows = vike_catalog::spelled(vec![
            vike_model::Instrument::new(
                VENUE,
                "BTC-USDT",
                "BTC",
                "USDT",
                vike_model::Contract::CryptoSpot,
            ),
            vike_model::Instrument::new(VENUE, "BTC-USDT-SWAP", "BTC", "USDT", perp),
            vike_model::Instrument::new(VENUE, "QTUM-USDT-261225", "QTUM", "USDT", future),
        ]);
        for row in &rows {
            let user = vike_catalog::user_name_of_row(row).expect("a user name");
            let class = InstrumentType::of_inst_id(&row.raw_symbol);
            let is_spot = row.asset_class() == AssetClass::CryptoSpot;
            assert_eq!(class == Some(InstrumentType::Spot), is_spot, "{}", row.raw_symbol);
            assert_eq!(spot.spelling.names_spot(&user), is_spot, "{user}");
            assert_eq!(spot.spelling.names_derivative(&user), !is_spot, "{user}");
        }
    }

    /// The venue's own `expTime` of the three dated futures the demo listed on 2026-10-30.
    #[test]
    fn the_delivery_instant_is_the_venues_exp_time() {
        for (id, exp_time) in [
            ("QTUM-USDT-261225", 1_798_185_600_000_i64),
            ("GRASS-USDT-261106", 1_793_952_000_000),
            ("XRP-USD-270326", 1_806_048_000_000),
            ("BTC-USD-271231", 1_830_240_000_000),
            // A leap day, and a day the month lacks, which the shape check admits and the calendar rolls into
            // 3 March (the arithmetic's own answer, pinned as it stands).
            ("BTC-USD-280229", 1_835_424_000_000),
            ("BTC-USD-260231", 1_772_524_800_000),
        ] {
            assert_eq!(future_expiry_ms(id), Some(exp_time), "{id}");
        }
        assert_eq!(future_expiry_ms("BTC-USDT-SWAP"), None);
        assert_eq!(future_expiry_ms("BTC-USDT"), None);
    }

    /// The guard reads the USER name's day at okx's hour (`vike_venues::venue_delivery_guard`): the same instant as
    /// the instId's `expTime` (the measurement pin `crate::catalog`'s `the_name_rule_agrees_with_every_row` holds).
    #[test]
    fn an_opening_order_is_refused_in_the_last_day_and_everything_after_the_delivery() {
        let user = "QTUMUSDT_261225";
        let expiry = future_expiry_ms("QTUM-USDT-261225").unwrap();
        let window = delivery_guard_for(VENUE).window_ms.expect("okx guards its dated futures");
        // far from the delivery: nothing is refused
        assert_eq!(expiry_refusal(user, expiry - 30 * 86_400_000, false), None);
        // just outside the guard
        assert_eq!(expiry_refusal(user, expiry - window - 1, false), None);
        // inside the guard: an opening order is refused in words, a reduce-only order is taken
        let why = expiry_refusal(user, expiry - 3 * 3_600_000, false).expect("refused");
        assert!(why.contains("OPENING") && why.contains("3 h") && why.contains(user), "{why}");
        assert_eq!(expiry_refusal(user, expiry - 3 * 3_600_000, true), None);
        // at and after the delivery everything is refused
        for reduce_only in [false, true] {
            let why = expiry_refusal(user, expiry, reduce_only).expect("delivered");
            assert!(why.contains("delivered"), "{why}");
        }
        // another class is never refused
        assert_eq!(expiry_refusal("BTCUSDT.P", expiry + 1, false), None);
        assert_eq!(expiry_refusal("BTCUSDT", expiry + 1, false), None);
    }

    #[test]
    fn the_td_mode_refusal_is_exactly_the_measured_one() {
        assert!(is_td_mode_refusal(51000, "Parameter tdMode error "));
        assert!(!is_td_mode_refusal(51000, "Parameter sz error"));
        assert!(!is_td_mode_refusal(51020, "tdMode"));
        assert_eq!(SpotMode::Cross.other(), SpotMode::Cash);
        assert_eq!(SpotMode::Cash.wire(), "cash");
    }

    fn fill(symbol: &str, side: i32, qty: f64, px: f64, fee: f64, currency: &str) -> FillEvent {
        FillEvent {
            trade_id: TradeId::new("1").unwrap(),
            order_client_id: "c".into(),
            venue: "okx".into(),
            symbol: symbol.into(),
            side,
            last_qty: qty,
            last_price: px,
            commission: fee,
            commission_asset: currency.into(),
            liquidity_side: LiquiditySide::Taker,
            timestamp: 0,
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

    /// The grid every spot venue walks (`vike_bridge_core::spot_fee_pins`), by this venue's rule.
    #[test]
    fn spot_fee_grid_is_the_pinned_one() {
        vike_bridge_core::spot_fee_pins::assert_spot_fee_grid(
            include_str!("../tests/fixtures/spot_fee_grid.txt"),
            |side, qty, px, fee, asset| {
                spot_buy_net("HYPE-USDC", side, qty, px, fee, asset).unwrap_or((
                    qty,
                    fee,
                    asset.to_string(),
                ))
            },
        );
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
