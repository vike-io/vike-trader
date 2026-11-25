//! The node wire value types: a STANDALONE serde mirror of the rendered core snapshot
//! ([`CoreSnapshotWire`]) and of the order-write command vocabulary ([`CommandWire`]); the account
//! admin plane (decision 0065) lives in the private `account` child, re-exported here by name. An
//! operator's read of an account at its venue crosses as `vike_model::VenueRead` itself
//! ([`crate::proto::Response::VenueRead`]).
//!
//! # Why standalone mirrors (not the core types)
//!
//! These re-declare the field shapes of `vike_execution::{CoreSnapshot, OrderView, PositionView}`,
//! `vike_execution::{Command, OrderIntent}` and `vike_model::OrderRequest` instead of depending on
//! `vike-core`/`vike-execution`: the LIGHT thin-client crate stays at serde + framing + auth crypto and
//! `vike-model`'s serde vocabulary, and the mirror IS the schema, so a core refactor cannot silently
//! change it (a server projects `CoreSnapshot -> CoreSnapshotWire` at its edge). The tradeoff: a field the
//! GUI must see is added here too, on purpose, versioned by [`crate::proto::NODE_PROTO_VERSION`].
//!
//! ⚠ **A wire type that IS its `vike-model` type is that type, not a mirror** (the owner's rulings 14,
//! E8 and 15, 2026-11-11; decision 0091's last section): a combo's leg (`vike_model::OrderComboLeg`,
//! [`OrderComboWire::legs`]), the trading state (`vike_model::TradingState`), a limit's attributes and a
//! stop's adjustment (`vike_model::{OrderAttributes, OrderStopAdjust}`), an order list's contingency
//! (`vike_model::ContingencyType`), the order client id's shape (`OrderClientIdShape`) and the operator's venue
//! read (`vike_model::VenueRead` and its parts). Each replaced a field-for-field copy with the same
//! bytes. A field such a type grows is a field of this wire: where the type refuses an unknown key it
//! is refused (never dropped) by a node that does not know it, so it owes a feature word.
//!
//! # The additive-field contract
//!
//! A later field carries `#[serde(default)]` (usually with `skip_serializing_if`): an old node's
//! frame still decodes and a client that says nothing sends the bytes it always sent. No
//! `NODE_PROTO_VERSION` bump: the version is in the signed auth message, so a bump fails the
//! handshake against every running node. ⚠ Absent/empty means "the node did not say"; where it
//! matters check the `Welcome.features` capability, never the emptiness.

use serde::{Deserialize, Serialize};
use vike_model::accounts::account_keys::{AccountLabel, label_of_route_key, route_key_of};
use vike_model::orders::client_id::OrderClientIdShape;
use vike_model::{
    ArmingBlock, ContingencyType, OrderAttributes, OrderComboLeg, OrderStopAdjust, TradingState,
};

mod account;

pub use account::{
    AccountDirectoryAccountWire, AccountDirectoryVenueWire, AccountDirectoryWire, AccountListWire,
    AccountRequestWire, AccountRowWire, AccountVerbWire, AccountWriteWire, CredentialsHeldWire,
};

/// One order — mirrors `vike_execution::OrderView`. `status` is the rendered `OrderStatus` string, so
/// this crate need not mirror that enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderViewWire {
    pub order_client_id: String,
    pub venue: String,
    /// WHICH ACCOUNT's book this order rests in. ⚠ Absent = the default account OR a node that
    /// predates the field. The tell: [`VenueBlockWire::tier`] (the account's tier word, which
    /// replaced the `mode` of the release that first named orders' accounts) — blocks carrying a
    /// `tier` (PRESENCE, not value) mean orders name their account; otherwise, on a venue with
    /// several blocks, the order's account is UNKNOWN — never the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub symbol: String,
    /// +1 buy / -1 sell.
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    /// Rendered order status. The node sends the FSM word (`OrderStatus::as_str`: `"ACCEPTED"`,
    /// `"FILLED"`); a node built before that sent the Debug spelling (`"Accepted"`, `"Filled"`), so a
    /// reader must accept BOTH (version skew over the wire).
    pub status: String,
    pub venue_order_id: Option<String>,
    pub filled_qty: f64,
    pub average_fill_price: f64,
    /// The delay label of the data the order was priced off, as the node recorded it at pre-trade
    /// (decision 0140): `"real-time"`, `"delayed"` or `"frozen"` (`vike_model::StreamDelay::word`).
    /// Absent = no label stated, or a node that predates the field. Shown, never a gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_delay: Option<String>,
    /// WHY the node refused the order (the `reason` of its `OrderRejected`/`OrderDenied`). Absent =
    /// not refused, no reason recorded, or a node that predates the field - additive, so an older
    /// reader ignores it and a newer reader of an older node sees `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// WHICH refusal [`Self::reason`] is: the catalog id its refusing event carried (`vike_execution::OrderView::refusal_id`),
    /// shown beside the reason as `error [<id>]: <text>` (`vike_utility::errors::render_refusal_or_text`), never decided
    /// on. A READING, no feature word: absent = no id (not refused, a producer that sets none) or a node that predates the
    /// field, and an older reader ignores the key (`wire_tests.rs`'s `a_refusal_id_is_additive_and_old_and_new_observers_read_each_other`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_id: Option<String>,
    /// **How long this NODE PROCESS has known the order, in milliseconds, as of the frame** - stamped
    /// by the publisher (`crates/vike-tradehub/src/publish.rs`'s `NodeReadings::stamp`) off the fold, for
    /// an order that is not terminal. ⚠ NOT the order's age at the venue: the clock starts when this
    /// process first SAW the order, so after a restart an order that survived reads no older than the
    /// node's uptime ([`CoreSnapshotWire::uptime_ms`]). Absent = a terminal order, or a node that predates
    /// the field: render unknown, never zero (a missing age read as 0 would call every order new).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
    /// A resting trailing order's TRAIL, in price (its activation is [`Self::trigger_price`]); absent on every
    /// other kind, so a non-trailing order is the frame from before. A READING: it shipped with
    /// [`crate::proto::FEATURE_ORDER_TRAILING`] before any node advertised it, so no word of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<f64>,
    /// A resting `trailing_limit` order's LIMIT OFFSET, in price; absent on every other kind, as [`Self::trail`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_offset: Option<f64>,
    /// A resting limit order's ATTRIBUTES ([`OrderRequestWire::attributes`]). A READING (absent when `None`, no word),
    /// read by [`or_not_said`] so a member a newer node adds never fails an older client's frame.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_not_said")]
    pub attributes: Option<OrderAttributes>,
    /// A resting trailing order's PERCENT trail ([`OrderRequestWire::trail_percent`]); a READING, as [`Self::trail`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_percent: Option<f64>,
    /// A resting stop's ADJUSTMENT not yet made ([`OrderRequestWire::adjust`]); a READING, as [`Self::attributes`].
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_not_said")]
    pub adjust: Option<OrderStopAdjust>,
    /// The RELEASE of an order that rests at its venue with it (a time release a venue holds itself, IBKR's
    /// `goodAfterTime`); a READING, as [`Self::attributes`].
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_not_said")]
    pub release: Option<OrderReleaseWire>,
    /// The quantity a resting order SHOWS at its venue ([`OrderRequestWire::display_quantity`]): an iceberg's slice, zero
    /// for a HIDDEN order. A READING, as [`Self::trail_percent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_quantity: Option<f64>,
}

/// One net/hedge position leg — mirrors the display subset of `vike_execution::PositionView`.
/// `position_side` is the leg label (`"BOTH"`/`"long"`/`"short"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionViewWire {
    pub venue: String,
    pub symbol: String,
    pub position_side: String,
    pub size: f64,
    pub average_price: f64,
    pub unrealized: f64,
    /// Effective leverage (`1/im`); 0.0 when the margin gate is off.
    pub leverage: f64,
    /// Estimated liquidation mark; 0.0 when not liquidatable / gate off.
    pub liquidation_price: f64,
    /// Which source PRICED `unrealized` (`vike_execution::PriceSource::as_str`: `mark`, `last_trade`, ...),
    /// absent for a position the node's board has no price for, whose `unrealized` reads 0.0 like a flat
    /// P/L. A READING, no feature word: a node that predates it sends no key, and that frame reads every
    /// position as not priced, which is what the desktop read before the key existed; a string the
    /// reader does not take reads as not said ([`word_or_not_said`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "word_or_not_said"
    )]
    pub mark_source: Option<String>,
}

/// One order the node HOLDS off the venue — mirrors `vike_execution::OrderHeldView`: a PENDING bracket exit until its
/// OTO parent fills (`parent_order_id` names the entry whose fill releases the leg), or an order held until its
/// [`Self::release`]. NOT in [`CoreSnapshotWire::orders`] (no venue order yet). `cancel <order client id>` cancels either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderHeldViewWire {
    pub order_client_id: String,
    pub venue: String,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    pub parent_order_id: Option<String>,
    /// WHICH ACCOUNT's book the held order goes to, as [`OrderViewWire::account`] (absent on a bracket exit, whose
    /// account is its entry's). A READING: absent when `None`, no feature word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// WHEN the node sends it (`OrderRequestWire::release`): present on an order held by time (and price), absent
    /// on a bracket exit and from a node that predates the field. A READING: an `Option`, no feature word
    /// (`crates/vike-tradehub-client/CLAUDE.md`'s rule for a reading that rides a reply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<OrderReleaseWire>,
    /// Why an order whose release time came is STILL held: its price condition has no fresh price to read
    /// (`vike_execution::OrderHeldView::waiting`), in the node's words. A READING beside [`Self::release`], never a key
    /// inside `OrderReleaseWire` (whose `deny_unknown_fields` would fail an older client's whole frame): absent when
    /// `None`, no feature word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    /// The venue whose price [`Self::release`]'s condition reads when it is not the order's own
    /// (`vike_model::OrderReleasePrice::venue`). A READING beside the release, never [`OrderReleaseWire::venue`] here (an
    /// older client's `deny_unknown_fields` there would fail its whole frame): absent when `None`, no word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_venue: Option<String>,
    /// The held order's terms, READINGS as [`OrderViewWire`]'s of the same names: absent when `None`, no feature word,
    /// and a member a newer node adds inside one reads as not said ([`or_not_said`]), never a failed frame.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_not_said")]
    pub attributes: Option<OrderAttributes>,
    /// As [`OrderViewWire::trail_percent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_percent: Option<f64>,
    /// As [`OrderViewWire::adjust`].
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_not_said")]
    pub adjust: Option<OrderStopAdjust>,
    /// As [`OrderViewWire::display_quantity`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_quantity: Option<f64>,
}

/// A READING's reader that never fails the frame: a value of the expected shape is `Some`; any other shape, a key a
/// newer node added inside it, `null` and an absent key are `None` ("the node did not say"), as
/// [`word_or_not_said`] is for a string.
fn or_not_said<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

/// A STRING field's reader that never fails the frame: a string is `Some`; any other value or
/// shape, `null` and an absent key are `None` ("the node did not say"). ⚠ Not the derived reader:
/// that fails the WHOLE frame on a shape it does not expect, and the desktop's observe loop ends the
/// link on an undecodable frame — so one odd value from a newer node would silently drop every
/// older desktop. Serialisation is unchanged.
fn word_or_not_said<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| match value {
        serde_json::Value::String(word) => Some(word),
        _ => None,
    }))
}

/// [`ArmingNotRunningViewWire::block_word`]'s writer: the block's KEY (`ArmingBlock::word`), the bytes the field carried as a
/// string.
fn block_as_word<S: serde::Serializer>(
    block: &Option<ArmingBlock>,
    out: S,
) -> Result<S::Ok, S::Error> {
    block.map(ArmingBlock::word).serialize(out)
}

/// [`ArmingNotRunningViewWire::block_word`]'s reader, which never fails the frame: a word read back by
/// `ArmingBlock::from_word`, a word a newer node knows and this build does not as `ArmingBlock::AdapterFailed` (the
/// model's rule: an unknown cause never reads as a block that runs), and anything else as not said
/// ([`word_or_not_said`]).
fn block_or_not_said<'de, D>(deserializer: D) -> Result<Option<ArmingBlock>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(word_or_not_said(deserializer)?
        .map(|word| ArmingBlock::from_word(&word).unwrap_or(ArmingBlock::AdapterFailed)))
}

/// What one unit of one symbol's quantity is worth — the NON-DEFAULT rows of an engine's
/// `vike_execution::VenueBlock::contract_values` grid (decision 0170), so a remote client's order preview
/// sizes by the same number the node's gate does. Carried as text and a number: `kind` is the
/// `NotionalKind` word (`"linear"`/`"inverse"`), parsed at the client's edge, where a kind the reader
/// does not know drops that one row instead of failing the frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractValueWire {
    /// The symbol the row is for.
    pub symbol: String,
    /// `"linear"` or `"inverse"` (`vike_model::NotionalKind`'s serde word).
    pub kind: String,
    /// Base units (linear) or quote-currency units (inverse) one unit of quantity stands for.
    pub per_unit: f64,
}

/// A per-venue ledger block — mirrors the display fields of `vike_execution::VenueBlock`; the heavy
/// internals (fee schedule, margin math) stay core-side, and the contract-value grid rides as its
/// non-default rows ([`ContractValueWire`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueBlockWire {
    pub venue: String,
    pub balance: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub funding_paid: f64,
    /// Mode-aware, resolver-priced equity at this venue's seed.
    pub equity: f64,
    /// Resolver-priced unrealized total for this venue.
    pub unrealized: f64,
    /// Open positions on this venue with no priceable source.
    pub missing_prices: u32,
    /// Margin locked by open positions (0.0 when the gate is off).
    pub margin_used: f64,
    /// Free buying power (equals `equity` when the gate is off).
    pub free_buying_power: f64,
    pub trading_state: TradingState,
    pub positions: Vec<PositionViewWire>,
    /// WHICH ACCOUNT of `venue` this block is; absent for the unlabelled one (the
    /// `vike_model::accounts::account_keys::AccountLabel` convention). Mirrors
    /// `vike_execution::VenueBlock::account`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// This engine's ROUTING key: the bare venue id for the default account, else `venue#LABEL`.
    /// ⚠ **THE GATE COMPARES AGAINST THIS, not [`Self::venue`]**
    /// (`docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` verdict 2): two
    /// accounts of one venue share `venue` and differ here. Empty = node predates account routing.
    #[serde(default)]
    pub route_key: String,
    /// The symbols this account's engine trades, primary first. ⚠ EMPTY = the node did not say,
    /// never "trades nothing" (that reading would refuse every symbol on an older node).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<String>,
    /// **The tier of the account this block's engine runs** — the WORD (`paper`, `demo`, `live`:
    /// `vike_model::AccountTier::as_str`, decision 0156), carried raw like
    /// [`AccountDirectoryAccountWire::tier`] (decision 0129 item 7, as amended 2026-11-11).
    /// An account runs at its tier or not at all, so it is also what an order sent to it goes to.
    ///
    /// ⚠ `None` = the node did not say: render unknown and treat as `live` wherever safety is at
    /// stake, never guess. A word the reader's parser refuses reads the same. ⚠ Its PRESENCE is
    /// the tell that this node names each ORDER's account ([`OrderViewWire::account`]).
    ///
    /// The key was `mode` and its type `WireEngineMode`, a second three-word enum naming what an
    /// engine's orders went to while a paper engine could stand in for an account; decision 0156
    /// deleted both — the wire is text, and the tier is the account's.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "word_or_not_said"
    )]
    pub tier: Option<String>,
    /// **The engine's contract values that are not the default** (decision 0170): one row per symbol
    /// whose value is not `Linear` 1.0. EMPTY = every symbol reads the default, which is exactly what
    /// a node that predates the field implies, so an older node's frame decodes to today's
    /// behaviour (the preview sizes at the default and the node's own gate stays the enforcing one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contract_values: Vec<ContractValueWire>,
    /// **This engine takes an order on a symbol it was not started on** (decisions 0173/0174: it
    /// admits the symbol on the first order). Mirrors `vike_execution::VenueBlock::admits_symbols`, which
    /// the node derives from the SAME table its control door admits by.
    ///
    /// The capability a client gates "trade any symbol" on. ⚠ Unlike the fields above, `false` is
    /// the SAFE reading of an absent key (an older node signs a foreign symbol on the MOUNTED
    /// symbol behind a plain Ack), so no feature string is needed: a client that sees `false` keeps
    /// the account to its one symbol, whichever of "this venue does not" and "this node predates
    /// it" is the cause. Absent from the frame when false, so a block that offers nothing is
    /// byte-identical to the one an older node writes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub admits_symbols: bool,
    /// **The conversion rate of every currency this account holds a symbol quoted in** (decision 0183, phase
    /// 1): one row per quote currency in use, with its age and whether it has gone stale. Mirrors
    /// `vike_execution::VenueBlock::quote_rates`. [`Self::contract_values`] already carries the rate multiplied in
    /// (the EFFECTIVE value); this table says WHICH rate that was and lets a client show that it is stale
    /// (an opening order on such a symbol is denied while it is). EMPTY for every account without a symbol
    /// quoted in another currency than its own, which is every account on a venue quoted in its own, and
    /// exactly what a node that predates the field implies; absent from the frame when empty, so such a block
    /// is byte-identical to the one an older node writes. An older client ignores the key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quote_rates: Vec<QuoteRateViewWire>,
    /// The currency [`Self::quote_rates`] is expressed in (the engine's ledger currency: the account's own).
    /// EMPTY exactly when the table is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote_rates_in: String,
    /// **The currency GROUP this block's money is in** (decision 0183, item 13): mirrors
    /// `vike_execution::VenueBlock::ledger_currency`. EMPTY for the dollar group (`USD`, `USDT`, an account whose
    /// currency nobody told: every account of today's nodes), else the upper-cased code (`"EUR"`). A
    /// total across blocks may add only the blocks of one group, so [`CoreSnapshotWire::equity_total`] is the
    /// PRIMARY block's group (`venues[0]`'s) and a reader that sums blocks itself groups by this key.
    ///
    /// ⚠ **No feature string, by decision 0112 verdict 3**: this is a pure addition to a REPLY, written
    /// only when non-empty, so a block of the dollar group is byte-identical to the one an older node
    /// writes and an older client ignores the key. An ABSENT key reads as the dollar group, which is
    /// the only reading the frame offers, not a statement that the node held only dollars: v0.1.79 to
    /// v0.1.81 told an OANDA demo account with an exposure ceiling its currency, so an older node CAN hold
    /// a EUR ledger beside USDT. A new observer on it reads ONE group and adds them as v0.1.81's observer
    /// did (no regression), and an older observer on a new multi-currency node reads the header from the
    /// primary group but still adds the table (better than before, still not right).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ledger_currency: String,
    /// **`true` when the currency this block's ledger is anchored to is only the owner's ASSUMED USD**, not the
    /// venue's word (decision 0189): mirrors `vike_execution::VenueBlock::currency_assumed`. `false` is the safe reading of
    /// an absent key (an older node), not a statement that the currency was stated: an account that is not
    /// anchored at all says `false` too. It says how the label was decided and stays `true` for the engine's
    /// life (the venue confirming it later lifts the balance hold, a separate row). Presentation only, an optional field with no feature string
    /// (decision 0112 verdict 3), absent from the frame when false so a block that assumes nothing is
    /// byte-identical to the one an older node writes; an older client ignores the key.
    #[serde(default, skip_serializing_if = "is_false")]
    pub currency_assumed: bool,
    /// **`true` when this block's `balance` is no venue balance**: the engine keeps its own book (`vike_execution::BalanceMode::Delta`),
    /// so `balance` is what it has paid in fees and funding since it started, and `equity` its configured capital plus that and
    /// its PnL. So is a paper account, and an inverse engine whose wallet is a coin the ledger never takes in (decision 0170):
    /// deribit reads `-0.155` beside a wallet of 999.99 ETH. Absent when false, so every block that holds the venue's figure is
    /// byte-identical to the one an older node writes; `false` from an older node says nothing either way.
    #[serde(default, skip_serializing_if = "is_false")]
    pub balance_is_own_book: bool,
    /// **The account code a client stamps into an order client id for this block's account** (decision 0270: two
    /// `[0-9A-Z]` characters, base36 of its vike.db `account.id`), mirroring `vike_execution::VenueBlock::account_code`.
    /// A READING, no feature string (decision 0112 verdict 3): absent = not said (an older node, or an engine
    /// nobody told its code), and a client then mints `00`, a label that decides nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_code: Option<String>,
    /// **The id shape a client mints for this block's account** (decision 0270), mirroring
    /// `vike_execution::VenueBlock::order_client_id_shape`. A READING like [`Self::account_code`]: absent = not said, and a client
    /// mints the full shape (a venue that cannot take it receives it bare: the Binance bridge never truncates).
    /// The model's `OrderClientIdShape` itself, which has no `deny_unknown_fields`: a key a later node adds never fails the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id_shape: Option<OrderClientIdShape>,
}

/// One row of [`VenueBlockWire::quote_rates`]: what one unit of `currency` is worth in the account's currency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteRateViewWire {
    /// The quote currency of one or more symbols of the account (`"JPY"`).
    pub currency: String,
    /// Account-currency units per ONE unit of `currency` (USDJPY on a USD account: about `1/150`).
    pub rate: f64,
    /// When the node observed it, epoch ms on the node's clock.
    pub as_of_ms: i64,
    /// Older than the engine's staleness window at the instant of this frame, or never priced.
    pub stale: bool,
    /// A position is open in a symbol quoted in `currency` (mirrors `vike_execution::QuoteRateView::held`, set only on a
    /// row that is `stale`): the stale rate is valuing money, not only refusing a new order. `false` is the
    /// safe reading of an absent key (an older node), and the key is absent from the frame when false, so a row
    /// that holds nothing is byte-identical to the one an older node writes. Additive, no feature string
    /// (decision 0112 verdict 3); an older client ignores the key.
    #[serde(default, skip_serializing_if = "is_false")]
    pub held: bool,
}

/// `route_key`, an engine of `venue`, as the book an operator types: `venue/LABEL`, and `venue/DEFAULT` for the
/// unlabelled account. The `#` rule is the model's (`label_of_route_key`), never re-spelled here; a key that names
/// no account of `venue` is printed as it came.
#[must_use]
pub fn book_of_route_key(venue: &str, route_key: &str) -> String {
    label_of_route_key(venue, route_key)
        .map_or_else(|| route_key.to_string(), |label| format!("{venue}/{label}"))
}

/// An account's route key from a venue and the operator's label, through the workspace's one
/// spelling. A label the grammar refuses can never have been mounted; it stays apart from the
/// default account rather than merging into it.
pub fn route_key(venue: &str, label: Option<&str>) -> String {
    route_key_of(venue, &account_label(label))
}

/// The operator's `label` as an account label: none is the default account, and one the grammar refuses is
/// kept as itself ([`route_key`]'s rule).
pub fn account_label(label: Option<&str>) -> AccountLabel {
    label.map_or(AccountLabel::Default, |l| {
        AccountLabel::parse(l).unwrap_or_else(|_| AccountLabel::Named(l.to_string()))
    })
}

/// The key a published block answers to; a node that predates route keys sends none.
pub fn block_key(block: &VenueBlockWire) -> String {
    if block.route_key.is_empty() {
        route_key(&block.venue, block.account.as_deref())
    } else {
        block.route_key.clone()
    }
}

/// A book as a command addresses it: `venue/ACCOUNT`, or the bare venue when it names none.
#[must_use]
pub fn book_of_address(venue: &str, account: Option<&str>) -> String {
    account.map_or_else(|| venue.to_string(), |account| format!("{venue}/{account}"))
}

/// The ONE wording of the refusal of a venue-scoped command that names no account where the venue runs more
/// than one (decision 0254), for the node's door and the clients that can tell first. `route_keys` are the
/// venue's engines, a lane twin's included: a bare venue does not count it as an account, and a book that names
/// it is still one to choose.
#[must_use]
pub fn names_no_account_refusal(subject: &str, venue: &str, route_keys: &[&str]) -> String {
    let mut books: Vec<String> = route_keys.iter().map(|k| book_of_route_key(venue, k)).collect();
    books.sort_unstable();
    format!(
        "venue `{venue}` has more than one account on this node, running or not, and {subject} names none of them. It \
         was REFUSED and nothing was sent: an order or a close goes only to the one account it names. \
         Name one of these books: {}",
        books.join(", ")
    )
}

/// The ONE wording of a conversion rate on every operator surface (`vike-cli trade equity`, Telegram `/equity`
/// and the desktop's Account window), so a stale rate reads alike wherever it is shown: `1 JPY = 0.006667 USD`,
/// then ` STALE` when the node judged it past its staleness window, then what that means for the account when
/// a position is open in the currency. `account_currency` is the currency the rates are in
/// ([`VenueBlockWire::quote_rates_in`]; `vike_execution::VenueBlock::quote_rates_in` for the surfaces that read the
/// core's block); an empty one reads as "the account's currency". A rate that is not a positive number has no
/// figure to print and is always stale (the observer folds it so).
#[must_use]
pub fn quote_rate_phrase(
    currency: &str,
    rate: f64,
    stale: bool,
    held: bool,
    account_ccy: &str,
) -> String {
    let account = if account_ccy.is_empty() { "(the account's currency)" } else { account_ccy };
    let mut out = if rate.is_finite() && rate > 0.0 {
        format!("1 {currency} = {rate:.6} {account}")
    } else {
        format!("1 {currency} = no usable rate")
    };
    if stale {
        out.push_str(" STALE");
        if held {
            out.push_str(" (an open position is valued at it)");
        }
    }
    out
}

impl QuoteRateViewWire {
    /// [`quote_rate_phrase`] of this row.
    #[must_use]
    pub fn say(&self, account_ccy: &str) -> String {
        quote_rate_phrase(&self.currency, self.rate, self.stale, self.held, account_ccy)
    }
}

/// `skip_serializing_if` for a `bool` that is absent when `false`.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// **One account the node knows and does NOT run** — mirrors `vike_execution::ArmingNotRunningView`
/// (decision 0156: an account runs at the tier its active row states or not at all, never as a
/// paper engine in its place). It has no [`VenueBlockWire`] because no engine stands behind it; this
/// is what lets a client show WHY instead of a bare "not running".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmingNotRunningViewWire {
    /// The venue id.
    pub venue: String,
    /// WHICH ACCOUNT of `venue`; absent for the unlabelled one ([`VenueBlockWire::account`]'s
    /// convention).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The ROUTING key an order addressed to it names: the bare venue for the default account,
    /// else `venue#LABEL`.
    pub route_key: String,
    /// The tier WORD its active `account` row states (`paper`/`demo`/`live`); absent when no
    /// active row states exactly one. Read through the reader's one tier parser.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "word_or_not_said"
    )]
    pub tier: Option<String>,
    /// WHY it does not run, as the node's arming block words it (`vike_model::ArmingBlock::as_str`:
    /// `no credentials`, `flags.tradehub_live off`, …), for a person: shown as given, never compared
    /// by a program (a rewording changes it). [`Self::block_word`] is the key.
    pub reason: String,
    /// The arming block that stopped it, carried as its KEY (`vike_model::ArmingBlock::word`: `account_inactive`,
    /// `no_credentials`, …), stable across a rewording of [`Self::reason`]. A word this build does not know reads as
    /// `ArmingBlock::AdapterFailed` (a cause it cannot name, never a block that runs) and a value that is not a word
    /// as absent, so neither fails the frame ([`block_or_not_said`]). Absent when no block stopped it (a restart, a
    /// remount) and on an older node, so a reader that must decide falls back to the reason there.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "block_as_word",
        deserialize_with = "block_or_not_said"
    )]
    pub block_word: Option<ArmingBlock>,
    /// What lies behind an `adapter failed` [`Self::reason`], in the words the refusal used: that one word covers a
    /// lease another client holds, a version skew, a key the gatewayhub refuses and a venue that did not answer.
    /// Stamped by the node's publisher from its own record of the last refused start, so `vike_execution::ArmingNotRunningView`
    /// carries none (it lives in the core's closure). Absent for every other reason and on an older node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// WHICH refusal [`Self::reason`] is when it is the node's own sentence (a `tradehub.not_running.*` id), shown with
    /// it once ([`Self::say`]), never decided on ([`Self::block_word`] is the key). A READING, no feature word, as
    /// [`OrderViewWire::refusal_id`]: absent = a row an arming block stopped (its reason has no catalog id), or a node
    /// that predates the field, and an older reader ignores the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_id: Option<String>,
}

impl ArmingNotRunningViewWire {
    /// The row after its tier, the ONE wording every surface shows (`trade health`, `trade status`, the desktop through
    /// `crate::observe_decode::map_not_running`): the node's words under their one id when it sent one
    /// (`vike_utility::errors::render_refusal_or_text`), then what lies behind them when the node kept that.
    #[must_use]
    pub fn say(&self) -> String {
        let reason =
            vike_utility::errors::render_refusal_or_text(self.refusal_id.as_deref(), &self.reason);
        match &self.detail {
            Some(detail) => format!("{reason}: {detail}"),
            None => reason,
        }
    }
}

/// One OHLCV candle — mirrors the display subset of `vike_marketdata::Bar`. Short field names keep the
/// body small (a series carries up to ~300 of these).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarWire {
    /// Open-time epoch ms.
    pub timestamp: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    /// Volume.
    pub v: f64,
}

/// One `(venue, symbol, interval)` bar tail — mirrors `vike_execution::BarSeries`. `closed` is the
/// LAST-K closed bars (capped NODE-side: the core's own vec is unbounded); `forming` is the live
/// in-progress candle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarSeriesWire {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub closed: Vec<BarWire>,
    pub forming: Option<BarWire>,
}

/// The rendered core state view the node publishes — mirrors the display projection of
/// `vike_execution::CoreSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreSnapshotWire {
    /// Per-publish sequence number (GUI change detection).
    pub sequence: u64,
    /// Primary engine's venue.
    pub venue: String,
    /// Primary engine's symbol.
    pub symbol: String,
    /// Primary engine trading state.
    pub trading_state: TradingState,
    /// Primary venue cash balance.
    pub balance: f64,
    /// Cross-venue total equity (the core's `compensated_sum` `equity_total`): the blocks of the PRIMARY block's
    /// currency group only ([`VenueBlockWire::ledger_currency`]); on a node whose engines share one currency,
    /// every block.
    pub equity_total: f64,
    /// Per-venue ledger blocks (primary first, then extras in registration order).
    pub venues: Vec<VenueBlockWire>,
    /// Order registry (insertion order), spanning every engine.
    pub orders: Vec<OrderViewWire>,
    /// Top-level (primary-venue) positions — the mirror of `venues[0].positions`.
    pub positions: Vec<PositionViewWire>,
    /// Pending bracket exits held until their OTO parent fills; empty on non-bracket runs.
    pub held_exits: Vec<OrderHeldViewWire>,
    /// Recent delivered exec events (the bounded journal tail).
    pub recent_events: Vec<String>,
    /// Set once a handler panicked — the core is HALTED in safe-state.
    pub fault: Option<String>,
    /// Bounded bar tails per mounted series (LAST-K closed + forming), projected node-side. Empty
    /// when the node has no bars yet or predates the field.
    #[serde(default)]
    pub bars: Vec<BarSeriesWire>,
    /// Which daemon this is ([`HubIdentityWire`]); `None` from a node that predates the field.
    #[serde(default)]
    pub identity: Option<HubIdentityWire>,
    /// A digest of the node's MOUNTED ACCOUNT SET (`vike_execution::CoreSnapshot::accounts_epoch`),
    /// changing exactly when the set of route keys does. `0` = predates the field, or no accounts.
    ///
    /// ⚠ It answers ONE question: did the set move between a preview and its confirm? A client
    /// stamps it into the preview and refuses a stale confirm (*"the node's account set changed
    /// since your preview, re-preview"*). A DIGEST, not a counter, which would restart at zero with
    /// the daemon (`vike_core::snapshot::accounts_epoch_of`).
    #[serde(default)]
    pub accounts_epoch: u64,
    /// **Every account the node knows and does NOT run, each with why** ([`ArmingNotRunningViewWire`],
    /// decision 0156). Empty = every account the node knows runs, or a node that predates the
    /// field: a client then says only that an account it cannot find among [`Self::venues`] is not
    /// running, as it always did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_running: Vec<ArmingNotRunningViewWire>,
    /// **The ROUTE KEYS of the RUNNING accounts whose key in the settings database is no longer the key
    /// their session started with** ([`KEY_CHANGED_WARNING`] is the sentence to show for each). The session
    /// keeps signing with the old key until that account restarts: a credential verb that reaches the running
    /// gatewayhub reconnects it (decision 0287), and nothing else restarts it by itself, because stopping an
    /// account cancels its resting orders. Words only: no fingerprint and no value is on the wire. Empty =
    /// none changed, or a node that predates the field (byte-identical frames).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_changed: Vec<String>,
    /// **The RUNNING accounts whose venue balance the node HOLDS BACK from their engine** ([`BalanceWithheldWire`]):
    /// their equity is the engine's own book (seed + PnL), not the venue's wallet, until the hold ends. Words
    /// only. Empty = none held, or a node that predates the field.
    ///
    /// ⚠ **No feature string, decision 0112 verdict 3**: a pure addition to a reply, written only when
    /// non-empty, so a frame with nothing held is byte-identical to the one an older node writes and an older
    /// observer ignores the key (an older observer simply shows nothing, as it always did).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub balance_withheld: Vec<BalanceWithheldWire>,
    /// **The RUNNING accounts whose VENUE PATH is down right now** ([`VenueLinkDownWire`]): the engine runs and its
    /// lease to the gatewayhub is up, but the bridge's own path to the venue (Dukascopy's JForex sidecar, OANDA's transactions stream, cTrader's account session, the private stream of a venue on the shared user-data pump) is not,
    /// and each says what that costs its orders. Words and counters only. Empty = none is down, **or a node that predates
    /// the field, or a venue that reports no path** (the empty list is never a proof that a path is up:
    /// [`Self::venue_path_up`] is).
    ///
    /// ⚠ **No feature string, decision 0112 verdict 3**: a pure addition to a reply, written only when non-empty, so
    /// a frame with every path up is byte-identical to the one an older node writes and an older observer ignores
    /// the key (an older observer shows the engine as it always did).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub venue_link_down: Vec<VenueLinkDownWire>,
    /// **The route keys of the RUNNING accounts whose bridge said, on the node's last few asks, that its path to the
    /// venue is up**: the positive half of [`Self::venue_link_down`], what lets a surface say "up" for an account and not
    /// only "not reported down". Absent = not known: no fresh answer, a venue whose bridge reports no path, or a node that
    /// predates the field; never "down". No feature string (decision 0112 verdict 3): a pure addition to a reply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub venue_path_up: Vec<String>,
    /// **The RUNNING accounts whose trade link to the gatewayhub is not up right now** ([`TradeLinkDownWire`]): the
    /// engine runs and no order of the account reaches the gatewayhub, redialling or closed for good. It says nothing
    /// past the gatewayhub (that hop is [`Self::venue_link_down`]). Empty = every link is up, **or a node that predates
    /// the field** (check [`crate::proto::FEATURE_HUB_LINKS`], never the emptiness, to tell them apart).
    ///
    /// Written only when non-empty, so a frame with every link up is byte-identical to an older node's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trade_link_down: Vec<TradeLinkDownWire>,
    /// **The price feeds whose strategy lane to the datahub is lost right now** ([`PriceLinkDownWire`]): the strategies
    /// of that venue hear no quote, tick or bar. Same rule as [`Self::trade_link_down`]: written only when non-empty,
    /// and [`crate::proto::FEATURE_HUB_LINKS`] tells empty from an older node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub price_link_down: Vec<PriceLinkDownWire>,
    /// **How many price dials hold a session with the datahub right now.** An empty [`Self::price_link_down`] proves
    /// the datahub up only if at least one does: a flat box with nothing mounted rests every dial idle, and they all
    /// read the same with the datahub dead. `None` = not said (a node that predates the field, or one with no dial
    /// registry), which is never `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_dials_up: Option<u32>,
    /// **The operator's ARMED CORE-HELD reduce-only stops** ([`OrderEmulationStopViewWire`], decision 0199's amendment): a
    /// stop-loss on a venue whose capability row has no reduce-only stop, held by THE NODE, so `order ls` (the
    /// venue's orders) never shows it. Empty = none armed, **or a node that predates the field** (check
    /// [`crate::proto::FEATURE_CORE_HELD_STOPS`], never the emptiness, to tell them apart).
    ///
    /// ⚠ **A READING, so no verb and no version bump (decision 0112)**: a pure addition to a reply, written only
    /// when non-empty, so a frame with none armed is byte-identical to the one an older node writes and an older
    /// observer ignores the key. The journal does not move either.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub core_held_stops: Vec<OrderEmulationStopViewWire>,
    /// **Every order the node EMULATES** ([`OrderEmulationViewWire`], decision 0275), ONE list for every kind: the trigger
    /// kinds it holds until they fire (a core-held stop of [`Self::core_held_stops`] is one of these rows too) and the
    /// working kinds that keep one plain limit resting (that child is a row of [`Self::orders`] itself). Empty = none,
    /// **or a node that predates the field** (check [`crate::proto::FEATURE_EMULATED_ORDERS`], never the emptiness).
    ///
    /// ⚠ **A READING, so no verb and no version bump (decision 0112)**, the [`Self::core_held_stops`] rule: written
    /// only when non-empty, so a frame with none is byte-identical to an older node's and an older observer ignores
    /// the key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emulated_orders: Vec<OrderEmulationViewWire>,
    /// **Every reconcile divergence the core is HOLDING for an operator** ([`ReconcileAlertViewWire`]),
    /// the rows `trade reconcile ls` lists and `CommandWire::ReconcileConfirm` resolves by id. Empty =
    /// nothing is held, or a node that predates the field (check
    /// [`crate::proto::FEATURE_RECONCILE_VERBS`], never the emptiness).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconcile: Vec<ReconcileAlertViewWire>,
    /// **How long the node PROCESS has been running, in milliseconds, at the moment this frame was
    /// built** (the publisher's clock, from the process start - `crates/vike-tradehub/src/publish.rs`'s
    /// `process_started`). A reading of the frame, not of the identity block: it changes with every
    /// frame, and [`HubIdentityWire`] is the process-static block a client compares for equality.
    /// `None` = the node did not say (a node that predates the field): render unknown, never `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
    /// **Whether the HALT sentinel file exists right now** - the out-of-band kill switch every
    /// adapter checks at its submit seam (`vike_boot::halt`), re-read for every frame. Tri-state on
    /// purpose: `Some(true)` the file is there, so every order that opens or adds risk is refused
    /// whatever the engines' state; `Some(false)` it is not; `None` = the node did not say (it
    /// predates the field, or no sentinel path was handed to the publisher). ⚠ `None` is NOT
    /// `false`: reading an older node's silence as "no sentinel" would call an engaged kill switch
    /// clear, which is why this is an `Option` and not a `bool` that defaults to false (decision
    /// 0112 verdict 3: a pure addition to a REPLY needs no feature string, and an `Option` is what
    /// lets the reader tell the two apart without one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halt_sentinel: Option<bool>,
    /// **The PRIMARY currency group's key** (decision 0183, item 13): the currency [`Self::equity_total`] and
    /// every other cross-venue total of this frame are in. EMPTY for the dollar group (`USD`, `USDT`, an
    /// untold currency: every node today), else the upper-cased code. ⚠ A reader must take the primary from
    /// THIS, never from the first of [`Self::venues`]: the frame carries only the newest block of each
    /// account, so after the primary account stopped and started again the first block is another engine's
    /// (a `[binance USDT, oanda EUR, bybit USDT]` node whose binance restarted publishes `[oanda, bybit,
    /// binance']`), and its group is not the node's.
    ///
    /// No feature string, decision 0112 verdict 3: a pure addition to a reply, written only when non-empty.
    /// ABSENT reads as the dollar group: the only reading the frame offers (an older node CAN hold a EUR
    /// ledger beside USDT, see [`VenueBlockWire::ledger_currency`]; its blocks carry no key either, so the
    /// observer sees one group and adds them, as v0.1.81's did).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub primary_currency: String,
    /// When the node framed this snapshot, epoch ms on the node's clock: what a reader measures a frame's age by
    /// ([`crate::tradehub_handle::TradehubCoreHandle::frame_age`]). `0`, and absent from the frame, from a node that predates
    /// it. No feature string, decision 0112 verdict 3: a pure addition to a reply.
    #[serde(default, skip_serializing_if = "is_no_stamp")]
    pub published_ms: i64,
    /// **The core's health counters** ([`CoreCountersWire`]), since the node process started. A READING (decision 0112
    /// verdict 3: no feature string): `None` = the node did not say (it predates the field), which is never "all zero".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters: Option<CoreCountersWire>,
    /// **The node process's ERROR and WARN lines since it started, one row per target that logged one**
    /// ([`LogLinesWire`]; counted by `vike_log::line_counts`, the panic hook's line included), stamped by the publisher
    /// off the fold. A READING (decision 0112 verdict 3: no feature string): `None` = not said (a node that predates the
    /// field, or a process that counts nothing), which is never "no line logged"; `Some` and empty is that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_lines: Option<Vec<LogLinesWire>>,
    /// **How many alerts failed every ask to their webhook target since the node process started** (the alert mount's
    /// count, `vike_tradehub::alerts::AlertMount::deliveries_failed`), stamped by the publisher off the fold: each one
    /// is an alert the operator was never sent. A READING (decision 0112 verdict 3: no feature string): `None` = not
    /// said (a node that predates the field), never "none failed"; `Some(0)` is that, alerting mounted or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_deliveries_failed: Option<u64>,
    /// **The tradehub journal's size and growth at the node's last measure** ([`JournalWire`]: the journal watch,
    /// `crates/vike-tradehub/src/journal_watch.rs`'s `JournalWatch`, measures once a minute off the fold), stamped by the
    /// publisher. A READING (decision 0112 verdict 3: no feature string): `None` = not said (a node that predates the
    /// field, one with no `config.journal_dir` row, or one that has not measured yet), never an empty journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<JournalWire>,
}

/// The journal's reading on the frame ([`CoreSnapshotWire::journal`]). A key this build does not know is ignored, one
/// it misses reads as its default, so the block can grow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JournalWire {
    /// The bytes of every file under the journal directory but its archive.
    pub total_bytes: u64,
    /// Its top-level segments.
    pub segments: u64,
    /// How fast the total grew over the last hour, in bytes an hour; absent while the window is too short to judge (the
    /// first 15 minutes after a start, or after the total shrank).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_per_hour: Option<u64>,
    /// Whether the journal is over `config.journal_size_alert_mib` or `config.journal_growth_alert_mib_per_hour`: the
    /// node's own judgement against its rows, the latch its one `journal_growth` alert per crossing fires on.
    pub over: bool,
}

/// One target's lines on the frame ([`CoreSnapshotWire::log_lines`]): the module path that logged them, never their
/// text, so the rows are few and fixed. A key this build does not know is ignored, one it misses reads `0`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogLinesWire {
    pub target: String,
    pub errors: u64,
    pub warnings: u64,
    /// When its newest ERROR line was logged, epoch ms on the node's clock (compare it with
    /// [`CoreSnapshotWire::published_ms`], the same clock); `0`, and absent, when it logged none.
    #[serde(skip_serializing_if = "is_no_stamp")]
    pub latest_error_ms: i64,
}

/// The core's health counters on the frame: the mirror of `vike_execution::CoreCounters`, one key per name of
/// `vike_execution::COUNTER_NAMES` (`wire_tests.rs`'s `the_counter_keys_are_the_published_names` holds the two
/// together). A key this build does not know is ignored, one it misses reads `0` (`default`), so the block can grow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreCountersWire {
    pub market_updates_dropped_conflated: u64,
    pub commands_rejected_queue_full: u64,
    pub events_dropped_order_already_closed: u64,
    pub closing_events_dropped_order_still_live: u64,
    pub events_dropped_order_unknown: u64,
    pub events_dropped_number_not_finite: u64,
    pub restored_orders_adopted: u64,
    pub restored_orders_gone: u64,
    pub restored_orders_filled_while_down: u64,
    pub tags_repointed_order_still_live: u64,
}

/// `skip_serializing_if` for a stamp nobody made.
fn is_no_stamp(ms: &i64) -> bool {
    *ms == 0
}

/// One held reconcile divergence: the wire twin of `vike_execution::ReconcileAlertView`. Never the
/// proposed events themselves - those stay fold-thread state until an operator confirms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconcileAlertViewWire {
    /// The key `CommandWire::ReconcileConfirm` resolves by: a 48-bit digest of the divergence's
    /// identity (venue, account, kind, instrument), so the SAME divergence has the SAME id after a
    /// restart and a retried confirm can only reach it (or nothing). It was a per-run counter, and
    /// `confirm 3` after a restart landed on whatever divergence the new run held as #3.
    pub id: u64,
    /// WHICH BOOK the divergence is about: the account's route key (the bare venue id for a
    /// venue's default account).
    pub account: String,
    /// The `ReconcileDivergenceKind` name (`OrphanLocalPosition`, `BalanceDrift`, ...).
    pub kind: String,
    /// The operator-readable description.
    pub detail: String,
    /// How many events a confirm folds (0 for an acknowledgement-only or drop-only kind).
    pub proposed_event_count: usize,
}

/// The sentence an operator surface shows for a RUNNING account listed in [`CoreSnapshotWire::key_changed`].
pub const KEY_CHANGED_WARNING: &str =
    "key changed in the database - the old key is still in use until a restart";

/// One account whose venue balance the node holds back from its engine ([`CoreSnapshotWire::balance_withheld`]): the
/// node-side balance gate (`vike-tradehub`'s `venue_backend::gateway::balance_gate`) found the cash the venue reports
/// cannot be shown to be in the currency the engine's ledger is labelled with, so the engine's equity stays
/// its own book (seed + PnL) instead of the venue's wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceWithheldWire {
    /// The account's ROUTING key (the bare venue id for the default account, else `venue#LABEL`).
    pub route_key: String,
    /// WHY, in short words, shown as given: `assumed USD` (the venue stated no currency and the node
    /// assumed USD), `stated EUR not anchored` (the venue stated a currency the engine's ledger could not be
    /// anchored to), `untold foreign stock` (a stock in another currency the daemon was not told yet).
    pub reason: String,
}

/// One running account whose venue path is down ([`CoreSnapshotWire::venue_link_down`]): the gatewayhub's bridge reports
/// it through the lease's status, the node keeps the last answer and stamps it on every frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueLinkDownWire {
    /// The account's ROUTING key (the bare venue id for the default account, else `venue#LABEL`).
    pub route_key: String,
    /// Why, in the bridge's own words (for Dukascopy the sidecar's reason, or the last failed restart's).
    #[serde(default)]
    pub why: String,
    /// The failures since the path was last up: the loss itself counts as one (`VenueLinkDown::failed`), and every
    /// restart the bridge then tried that failed or said nothing in time adds one.
    #[serde(default)]
    pub failed_restarts: u32,
    /// The wait before the bridge's next try, in milliseconds, as of the node's last look (`0` = not said).
    #[serde(default)]
    pub retry_in_ms: u64,
    /// What this costs the account's orders, in the bridge's own words. Empty = the path carries the orders and the
    /// bridge refuses them meanwhile (Dukascopy's sidecar); OANDA's transactions stream and cTrader's session say their
    /// own. Written only when present, so a frame about a sidecar reads byte for byte as it did.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effect: String,
    /// Not the path but what stands before it: the lease is up and the gatewayhub has NO LIVE ADAPTER behind it (its
    /// bridge panicked, or a restarted gatewayhub refused to start it), so every order is refused and nothing here
    /// starts one. Absent when false, so a frame about a path reads as it always did.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_adapter: bool,
}

impl VenueLinkDownWire {
    /// What this outage costs the account's orders: the bridge's own words, or, when it said none, the one default
    /// sentence every surface and the node's log line say.
    #[must_use]
    pub fn effect_words(&self) -> &str {
        if self.effect.is_empty() {
            "orders are refused until its bridge has it back"
        } else {
            self.effect.as_str()
        }
    }

    /// `the venue path is DOWN: <why> (...); ...`, for a surface that already names the account (a `trade health` row's
    /// scope), so every operator surface words the outage alike. It promises no try the bridge did not state: `retry_in_ms`
    /// 0 is "not said" (OANDA's lane has ended, a cTrader session is being opened), so no clause names a next one, and
    /// with nothing failed either there is no clause at all.
    #[must_use]
    pub fn say(&self) -> String {
        let why = if self.why.is_empty() { "no reason given" } else { self.why.as_str() };
        let effect = self.effect_words();
        if self.no_adapter {
            return format!(
                "the gatewayhub has NO LIVE ADAPTER for this account ({why}): {effect}"
            );
        }
        let tries = match (self.failed_restarts, self.retry_in_ms) {
            (0, 0) => String::new(),
            (failed, 0) => format!(" ({failed} failure(s) so far)"),
            (failed, retry_ms) => format!(
                " ({failed} failure(s) so far, the next try in about {} s)",
                retry_ms.div_ceil(1000)
            ),
        };
        format!("the venue path is DOWN: {why}{tries}; {effect}")
    }
}

/// One running account whose trade link to the gatewayhub is not up ([`CoreSnapshotWire::trade_link_down`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeLinkDownWire {
    /// The account's ROUTING key (the bare venue id for the default account, else `venue#LABEL`).
    pub route_key: String,
    /// Why, in the link's own words (empty = not said yet).
    #[serde(default)]
    pub why: String,
    /// How long it has been down, in milliseconds, when this frame was built (`0` = not known).
    #[serde(default)]
    pub down_ms: u64,
    /// The lease is CLOSED FOR GOOD (taken over, refused): nothing redials it. Absent while it redials.
    #[serde(default, skip_serializing_if = "is_false")]
    pub closed: bool,
}

/// One venue's price feed whose strategy lane to the datahub is lost ([`CoreSnapshotWire::price_link_down`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceLinkDownWire {
    /// The venue id.
    pub venue: String,
    /// Why, in the dial's own words (empty = not said).
    #[serde(default)]
    pub why: String,
    /// How long it has been lost, in milliseconds, when this frame was built.
    #[serde(default)]
    pub down_ms: u64,
    /// How many venues' feeds want a lane at all, so a reader can tell "all of them" (the datahub) from "this one".
    #[serde(default)]
    pub of: u32,
}

/// One ARMED core-held reduce-only stop ([`CoreSnapshotWire::core_held_stops`]): the wire twin of
/// `vike_execution::OrderEmulationStopView`. The stop lives in the NODE's memory and journal, not at the venue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderEmulationStopViewWire {
    /// The arm id `cancel <id>` disarms.
    pub arm_id: String,
    pub venue: String,
    pub symbol: String,
    /// The account the stop fires onto: its route key (the bare venue id for the default account).
    pub account: String,
    /// `1` for a BUY stop (protects a short), `-1` for a SELL stop (protects a long).
    pub side: i32,
    pub qty: f64,
    pub trigger_price: f64,
    /// Restored from the journal and HELD until its account's first reconcile pass has landed (or the hold
    /// lapses): a crossing price does not fire it yet. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// How many times the stop was RE-ARMED after its close was refused or filled only in part. Absent at 0.
    #[serde(default, skip_serializing_if = "rearms_is_zero")]
    pub rearms: u8,
}

fn rearms_is_zero(n: &u8) -> bool {
    *n == 0
}

impl OrderEmulationStopViewWire {
    /// `SELL 2 X when the price crosses 95 (polymarket#ALT) [held ...]`, the ONE wording of a listed stop for
    /// every operator surface (`trade health`, `trade order stops`).
    #[must_use]
    pub fn say(&self) -> String {
        let mut line = format!(
            "{} {} {} {} when the price crosses {} ({})",
            self.arm_id,
            vike_model::side_word(self.side),
            self.qty,
            self.symbol,
            self.trigger_price,
            self.account
        );
        if self.held {
            line.push_str(
                "; HELD: restored from the journal, it fires only after this account's first reconcile pass",
            );
        }
        if self.rearms > 0 {
            line.push_str(&format!(
                "; re-armed {} time(s): the close it sent before was refused or filled only in part",
                self.rearms
            ));
        }
        line
    }
}

/// One EMULATED order ([`CoreSnapshotWire::emulated_orders`]): the wire twin of `vike_execution::OrderEmulationView`, an
/// order the NODE holds or works itself because its venue does not hold that kind. Every optional term is absent
/// when the kind states none, so a row carries only what it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderEmulationViewWire {
    /// What `cancel <id>` names: a trigger kind's arm id, a working kind's order client id.
    pub id: String,
    pub venue: String,
    /// The labelled account, as [`OrderViewWire::account`]; absent for the default account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub symbol: String,
    /// `1` buy, `-1` sell.
    pub side: i32,
    /// Its whole size.
    pub qty: f64,
    /// The kind's word: `stop`, `trailing`, `take_profit`, `stop_limit`, `trailing_limit`, `trailing_take_profit`
    /// (a trigger kind) or `chase`, `iceberg` (a working kind). A reader shows a word it does not know as it is.
    pub kind: String,
    /// It may only reduce the position it meets. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reduce_only: bool,
    /// A trigger kind's trigger level as it stands now (a trailing kind's moves with its extreme).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<f64>,
    /// The limit price: a stop-limit's, a limit-if-touched take-profit's, an iceberg's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    /// A trailing kind's distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<f64>,
    /// A chase's anchor and offset. A READING, read by [`or_not_said`] as [`OrderViewWire::attributes`] is: a member a
    /// newer node adds inside it (`OrderPegWire` refuses an unknown key) reads as not said, never a failed frame.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_not_said")]
    pub peg: Option<OrderPegWire>,
    /// An iceberg's visible size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_quantity: Option<f64>,
    /// A working kind's filled size and average price (`0` while nothing filled, and always for a trigger kind,
    /// which fills only as the order it fires).
    #[serde(default)]
    pub filled: f64,
    #[serde(default)]
    pub average_price: f64,
    /// A working kind's live child, a row of [`CoreSnapshotWire::orders`]; absent between two children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<String>,
    /// Why it acts on nothing now, in the node's words; absent while it works. On a trigger kind it is said only
    /// while the arm is HELD for its account's first reconcile pass (restored from the journal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    /// How many times a trigger kind was RE-ARMED after the close it sent was refused or filled only in part
    /// (`0` = as armed, and always for a working kind). Absent at 0.
    #[serde(default, skip_serializing_if = "rearms_is_zero")]
    pub rearms: u8,
    /// A trigger kind restored from the journal and HELD until its account's first reconcile pass: the core's own
    /// word (`vike_execution::OrderEmulationView::held`), never inferred from [`Self::waiting`]. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

impl OrderEmulationViewWire {
    /// This row as the CORE-HELD reduce-only stop it is (decision 0199's amendment): a trigger kind (it states a
    /// trigger) that may only reduce. `None` for every other row. The ONE way an operator surface reads a core-held
    /// stop, so `trade order stops` and `trade health` list the same stops as the EMULATED block and each once:
    /// the account as its route key, `held` as the core states it ([`Self::held`]).
    #[must_use]
    pub fn as_core_held_stop(&self) -> Option<OrderEmulationStopViewWire> {
        let trigger_price = self.trigger.filter(|_| self.reduce_only)?;
        Some(OrderEmulationStopViewWire {
            arm_id: self.id.clone(),
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            account: match &self.account {
                Some(label) => format!("{}#{label}", self.venue),
                None => self.venue.clone(),
            },
            side: self.side,
            qty: self.qty,
            trigger_price,
            held: self.held,
            rearms: self.rearms,
        })
    }
}

impl CoreSnapshotWire {
    /// The core-held reduce-only stops of [`Self::emulated_orders`] ([`OrderEmulationViewWire::as_core_held_stop`]), in
    /// list order. Read THIS, never [`Self::core_held_stops`], which stays on the wire for the readers that know only
    /// it (MCP, an older desktop).
    #[must_use]
    pub fn core_held_stops_listed(&self) -> Vec<OrderEmulationStopViewWire> {
        self.emulated_orders.iter().filter_map(OrderEmulationViewWire::as_core_held_stop).collect()
    }
}

/// What a core-held stop is, said once for every operator surface that lists them.
pub const CORE_HELD_STOP_MEANING: &str = "held in this NODE's memory and journal, not at the venue: it \
     protects only while the node runs, and not through an outage, a restart gap or a lost price feed";

/// What a held balance means for the account, said once for every operator surface.
pub const BALANCE_HELD_MEANING: &str = "the venue's cash is not adopted, so the engine's equity is \
     its own book (seed + PnL) until the hold ends";

impl BalanceWithheldWire {
    /// `balance held: <reason> - <meaning>`, for a surface that already names the account (a `trade health` row's scope).
    #[must_use]
    pub fn say(&self) -> String {
        format!("balance held: {} - {BALANCE_HELD_MEANING}", self.reason)
    }

    /// The line an operator surface prints for the account, so every surface words the hold alike.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{}: {}", self.route_key, self.say())
    }
}

impl CoreSnapshotWire {
    /// **The NOT RUNNING row of the account under `route_key`.** The key alone decides, though both sides carry a
    /// tier: a tier change lists the account under its NEW tier while the stopped engine's block keeps the OLD one
    /// (`vike-tradehub`'s `Watcher::stop_what_the_rows_stopped`), and stays so when the new tier's start is refused, so
    /// a match on the tier as well reads that engine as running.
    #[must_use]
    pub fn stopped_row(&self, route_key: &str) -> Option<&ArmingNotRunningViewWire> {
        self.not_running.iter().find(|row| row.route_key == route_key)
    }

    /// An empty placeholder (`seq: 0`) — the wire twin of `vike_execution::CoreSnapshot::empty`.
    /// [`crate::tradehub_handle::TradehubCoreHandle::snapshot`] returns it before the first frame, so a
    /// reader always has a value to render (the "connecting" state).
    pub fn empty() -> Self {
        CoreSnapshotWire {
            sequence: 0,
            venue: String::new(),
            symbol: String::new(),
            trading_state: TradingState::Active,
            balance: 0.0,
            equity_total: 0.0,
            venues: Vec::new(),
            orders: Vec::new(),
            positions: Vec::new(),
            held_exits: Vec::new(),
            recent_events: Vec::new(),
            fault: None,
            bars: Vec::new(),
            identity: None,
            accounts_epoch: 0,
            not_running: Vec::new(),
            key_changed: Vec::new(),
            balance_withheld: Vec::new(),
            venue_link_down: Vec::new(),
            venue_path_up: Vec::new(),
            trade_link_down: Vec::new(),
            price_link_down: Vec::new(),
            price_dials_up: None,
            core_held_stops: Vec::new(),
            emulated_orders: Vec::new(),
            reconcile: Vec::new(),
            uptime_ms: None,
            halt_sentinel: None,
            primary_currency: String::new(),
            published_ms: 0,
            counters: None,
            log_lines: None,
            alert_deliveries_failed: None,
            journal: None,
        }
    }
}

/// WHICH daemon a [`CoreSnapshotWire`] describes — name, mounted strategy, the live switch, build — so a
/// GUI holding several backends can label them. Additive (the module's additive-field contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubIdentityWire {
    /// Operator-facing daemon name (the profile file stem today).
    pub name: String,
    /// The mounted strategy's registry name.
    pub strategy: String,
    /// The strategy's effective params, rendered (`vike_tradehub::config::MountResolvedStrategy::effective_params`).
    pub params: String,
    /// `flags.tradehub_live`, the STOP for every `demo` and `live` account (decision 0156 item 4). ⚠ NOT what the node
    /// runs: `true` runs each account at its own tier, demo and paper included, and `false` runs only `paper` accounts,
    /// of which there may be none. [`crate::node_words::running_tiers`] is what runs. Which venues a node with it `false`
    /// prices is [`Self::priced_venues`] ([`Self::prices_venue`]).
    pub live: bool,
    /// The daemon's build identity (`vike_buildinfo::summary`).
    pub build: String,
    /// WHICH BOX the daemon runs on, as the daemon reports it: the client cannot derive it (behind
    /// the SSH tunnel every client dials `127.0.0.1:7879`). ⚠ A CLAIM, not a verified fact, naming
    /// a box rather than a dialable endpoint. Empty = nothing was said (older node, or a
    /// loopback-only box with `config.tradehub_advertise_addr` unset), never 127.0.0.1. Produced in
    /// `crates/vike-tradehub/src/self_address.rs`, rendered by
    /// `vike_app_core::backend::identity`.
    #[serde(default)]
    pub advertise_addr: String,
    /// **The venues this node dials a price feed for, when it states them**: the paper-only arm (`live` false) names the
    /// KEYLESS venues of its mounts whose feed it wired through the datahub (the owner's "3 - yes!", 2026-11-16), and
    /// an empty list when the price path was away at its start. `None`: not stated (a node with `live` on, which prices
    /// its mounted venues, or a paper-only node older than the field, which dialled none). Read through
    /// [`Self::prices_venue`]. Absent from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priced_venues: Option<Vec<String>>,
}

/// **What a node with NO price feed for `venue` says**, on every manual order it refuses and on every strategy mount of
/// that venue `trade health` lists (the owner's "never silent", 2026-11-16): a paper-only node (`flags.tradehub_live`
/// off, the default setup) prices only the keyless venues it wired, so an order or a mount of any other venue (a
/// credentialed one: its prices need its demo login, which this mode does not run) can never fill. One sentence, read
/// by the node's door and by `vike_client::health`.
#[must_use]
pub fn no_price_feed(venue: &str) -> String {
    format!(
        "no price: this node has no price feed for {venue} (paper-only mode, flags.tradehub_live off)"
    )
}

impl HubIdentityWire {
    /// **Whether this node dials a price feed for `venue`**: the venues it states ([`Self::priced_venues`]), else every
    /// venue on a node with `live` on and none on an older paper-only node (`crates/vike-tradehub/src/tradehub_cli.rs`'s
    /// `start_paper` dialled no feed). An order or a mount of a venue it does not price says [`no_price_feed`].
    #[must_use]
    pub fn prices_venue(&self, venue: &str) -> bool {
        match &self.priced_venues {
            Some(venues) => venues.iter().any(|v| v == venue),
            None => self.live,
        }
    }
}

/// One mounted strategy in a [`StrategyStatusWire`]: one row per mount the node runs NOW, the boot's in mount order and
/// the runtime ones after them. A mount unmounted has no row.
///
/// The addressing key a [`CommandWire::UpdateParams`] targets (`venue`/`symbol`/`interval`/
/// `mount_id`) is on the row, so no client string-parses `vike-tradehub`'s `mounts_wire_params`
/// prefix.
///
/// ⚠ Every field below `live` is `#[serde(default)]` and reads EMPTY from a node that predates
/// [`crate::proto::FEATURE_STRATEGY_PARAMS`], indistinguishable from "this mount publishes no
/// typed params": **check the capability, never the emptiness.**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountViewWire {
    /// The mounted strategy's registry name (e.g. `"spread_maker"`); a script path for a runtime mount of a Rhai
    /// strategy, and `"unknown"` for a runtime mount the node holds no record of.
    pub strategy: String,
    /// The mount's effective params, rendered (`vike_tradehub::config::MountResolvedStrategy::effective_params`, or the mount command's own table for a
    /// runtime mount). Boot-time prose, stale after the first `UpdateParams`: a read-modify-write reads
    /// [`Self::typed_params`].
    pub params: String,
    /// True iff this mount's orders reach a venue; false = paper, or its account does not run. ⚠ A per-VENUE fact from
    /// `vike_tradehub_core::assemble_runtime`'s `live_venues` (a real `ExecutionClient` was built), not the
    /// daemon's gate: a mount without credentials, `data_only`, or demoted to paper reads `false`
    /// while [`HubIdentityWire::live`] reads `true` — the normal, informative disagreement. The live overlay writes it
    /// again from [`Self::tier`] and [`Self::not_running`], and ⚠ a `demo` account reads `true`: only [`Self::tier`]
    /// tells demo from live.
    pub live: bool,
    /// This mount's VENUE (part of the [`CommandWire::UpdateParams`] key).
    #[serde(default)]
    pub venue: String,
    /// This mount's SYMBOL — see [`Self::venue`].
    #[serde(default)]
    pub symbol: String,
    /// This mount's INTERVAL — see [`Self::venue`]. Two mounts may differ only by it.
    #[serde(default)]
    pub interval: String,
    /// This mount's ID (`CommandWire::UpdateParams::mount_id`), the part of the key that separates
    /// two mounts on one series: the sanitized `controller_id` or the derived
    /// `{venue}__{symbol}__{interval}`. `""` from a node that predates
    /// [`crate::proto::FEATURE_PARAMS_BY_MOUNT`] and on a row derived from the identity block.
    #[serde(default)]
    pub mount_id: String,
    /// The mount's LIVE tunables (read off the live core) as `vike_model::StrategyParams` serde
    /// JSON — the exact shape [`CommandWire::UpdateParams`] takes back. `None` = EITHER no typed
    /// params (`UpdateParams` cannot address it either) OR a node predating the capability;
    /// `Welcome.features` tells them apart.
    #[serde(default)]
    pub typed_params: Option<serde_json::Value>,
    /// WHAT PRODUCT this mount trades, as the `vike_model::AssetClass::sql_word` (`"CryptoPerp"`,
    /// …; equal to its serde spelling, parse with `AssetClass::from_sql_word`). ⚠ `None` from a
    /// node predating [`crate::proto::FEATURE_MOUNT_CLASS`] looks like a current TOML-backed mount
    /// (`docs/decisions/0061-an-instrument-names-its-kind.md` Phase 5) or a runtime mount, whose command
    /// names no product: check the capability.
    /// ⚠ DISPLAY ONLY: per 0061 not a routing input.
    #[serde(default)]
    pub asset_class: Option<String>,
    /// `true` while this mount's per-mount BUDGET LATCH is spent: it breached its `MountBudget`, its orders were
    /// cancelled and its new intents are discarded until an operator clears it with
    /// [`CommandWire::UnlatchMount`] (`vike-cli trade strategy reset-budget`). Written onto the row by the server's live
    /// overlay from the core's own published mount row, like `venue`/`mount_id`. ⚠ `false` also reads from a node
    /// that predates [`crate::proto::FEATURE_MOUNT_UNLATCH`] (absent from the wire while false): **check the
    /// capability, never the `false`.**
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub latched: bool,
    /// **WHICH ACCOUNT this mount trades: its ROUTING key** (the bare venue id for the default account, else
    /// `venue#LABEL`, as on [`VenueBlockWire::route_key`]), from the boot's mount or the node's record of a runtime one:
    /// a mount on another account is another mount. Empty = a node that predates the field, a row derived from the
    /// identity block, or a runtime mount the node holds no record of.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub route_key: String,
    /// **The tier that account runs at NOW** ([`VenueBlockWire::tier`]'s word, off the newest engine block of
    /// [`Self::route_key`], written by the live overlay): a tier change restarts the account and the core moves the mount
    /// to the new engine, so nothing from the boot can say this. With [`Self::not_running`] it is the tier the account's
    /// row states instead. Absent = a node that predates the field, or no block yet: the tier is not stated, and
    /// [`Self::live`] cannot tell demo from live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// `true` while the node lists this mount's account NOT RUNNING ([`CoreSnapshotWire::not_running`]): every order of
    /// the mount is refused. ⚠ Only `true` is a claim: a node that predates the flag writes none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_running: bool,
    /// **The mount's strategy code** (decision 0270): the four `[0-9A-Z]` characters its order client ids carry, so a
    /// client can name the strategy behind an id it decodes. A READING, no feature string: absent = not said (an
    /// older node, or a mount with no code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id_code: Option<String>,
    /// WHICH refusal [`Self::params`] holds when the mount does not run because the node refused it (a failed build):
    /// its id, shown with it once ([`Self::shown_params`]), never decided on. A READING, no feature word, as
    /// [`OrderViewWire::refusal_id`]: absent = a mount that runs or is still building, a refusal with no id, or a
    /// node that predates the field, and an older reader ignores the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_id: Option<String>,
}

impl MountViewWire {
    /// [`Self::params`] as a person reads it: the refusal it holds under its one id
    /// (`vike_utility::errors::render_refusal_or_text`), else as given.
    #[must_use]
    pub fn shown_params(&self) -> String {
        vike_utility::errors::render_refusal_or_text(self.refusal_id.as_deref(), &self.params)
    }
}

/// The payload of a `Response::StrategyStatus`: the node's identity block plus its mounts.
/// `effective_params` is the daemon's one resolved-params line (the identity block's string), so a
/// client that only wants "what is this node running" skips the mounts.
///
/// ⚠ No `Eq` here or on [`MountViewWire`]: the row holds a [`serde_json::Value`] (same reason
/// [`CommandWire`] derives only `PartialEq`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyStatusWire {
    /// WHICH daemon answered — the same identity block every [`CoreSnapshotWire`] carries.
    pub identity: HubIdentityWire,
    /// The daemon's resolved effective-params line (`vike_tradehub::config::MountResolvedStrategy::effective_params`).
    pub effective_params: String,
    /// One row per strategy the node runs NOW, runtime mounts included.
    pub mounts: Vec<MountViewWire>,
}

/// One effective settings row — the wire rendering of `vike_config::show`'s `SettingsShowRow`, the rows
/// `vike-cli config show` prints. STRINGLY on purpose (no `vike-config` dependency); secret-shaped
/// values arrive already redacted by the shared builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsShowRowWire {
    /// The settings SECTION (`"policy"`, `"config"`, …), the key's first segment. ⚠ An older node
    /// may still send a FILE name (`"policy.toml"`, before `docs/decisions/0086`).
    pub section: String,
    /// The dotted key (`"config.tradehub_addr"`).
    pub key: String,
    /// The EFFECTIVE value, rendered; `""` = unset. Already redacted for a secret-shaped key.
    pub value: String,
    /// The layer that set it: `"default"`, `"db"`, `"env:VIKE_RECONCILE"`.
    pub origin: String,
    /// The CLI's READ cell: the consuming binary's short name, `"yes"` (a library reads it, or a
    /// `policy.*` ceiling that is enforced), `"NO"` (nothing reads — or, for a ceiling, enforces —
    /// it: a configured row with this cell is a misconfiguration), or `"?"` (no consumption table
    /// carries the key: no answer either way, and a defect in the tables rather than a fact about
    /// the key).
    pub read_by: String,
}

/// The payload of a `Response::SettingsShow`: the node's effective settings rows, from the same
/// `vike_config::show` builder `vike-cli config show` uses. The FILES half only: the env-registry
/// half (a per-venue `<set>`/`<unset>` credential grid) stays a local-box disclosure —
/// `crates/vike-tradehub/src/server/settings.rs`'s `SettingsShowSource` argues the scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsShowWire {
    /// The settings directory the node resolved at boot, rendered. `None` = compiled-in defaults
    /// (no project above its working directory).
    pub settings_dir: Option<String>,
    /// One row per typed settings key, sorted by key.
    pub rows: Vec<SettingsShowRowWire>,
    /// Why [`Self::rows`] are NOT what this box holds, when they are not: its settings database could
    /// not be read, or the store is unsound (`vike_config::Description::store_refusal`,
    /// `vike_config::Settings::seal_refusal`). Every row may then be a compiled-in default reading
    /// `origin: default`, a ceiling as absent, so a reader takes none of them for a stored value.
    /// `None`, and absent on the wire, from a node with nothing to say and from one too old to say it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// One order intent — mirrors the serde fields of `vike_model::OrderRequest`. An empty
/// `order_client_id` asks the runtime to mint one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRequestWire {
    #[serde(default)]
    pub order_client_id: String,
    pub venue: String,
    pub symbol: String,
    /// +1 buy / -1 sell.
    pub side: i32,
    pub qty: f64,
    /// `"market"` | `"limit"` | `"stop"` | `"stop_limit"` | `"take_profit"` | `"market_to_limit"` | `"trailing"` |
    /// `"trailing_limit"` | `"pegged"`, carried verbatim; the
    /// node reads it through `vike_model::OrderType::parse` (trimmed, case-insensitive) and refuses
    /// any other word, naming the accepted ones.
    pub order_type: String,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub trigger_price: Option<f64>,
    #[serde(default)]
    pub reduce_only: bool,
    /// WHICH ACCOUNT of `venue`; `None` = the sender named none.
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing**: a client naming no account must send a frame
    /// BYTE-IDENTICAL to the pre-field one. The three wire states
    /// (`vike_model::accounts::account_keys::parse_wire_account`): absent = *named nothing*,
    /// `"DEFAULT"` = *the unlabelled account*, a label = that account. Absence cannot be read as a
    /// default: an old node DROPS a field it does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The order's LIFETIME: `"day"` | `"ioc"` | `"fok"` | `"gtd"` | `"post-only"` | `"at-open"` | `"at-close"` (`vike_model::TimeInForce`'s
    /// words, read through `TimeInForce::parse`: trimmed, case-insensitive, any other word REFUSED by the
    /// node naming the accepted ones). `None` = the default `gtc`, and an absent key. A lifetime other than
    /// `gtc` belongs to a LIMIT order, an auction one to a market or limit order (`vike_model::lifetime_refusal`). Requires
    /// [`crate::proto::FEATURE_ORDER_TIME_IN_FORCE`], refused client-side otherwise.
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing**: a submit naming no lifetime is BYTE-IDENTICAL to the
    /// pre-field frame, and `default` is what lets this node read that old frame. An older NODE drops the
    /// key, which is why the client owes the feature string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<String>,
    /// The expiry of a `"gtd"` order, epoch milliseconds UTC; nothing else takes one. Same gate and the
    /// same absent-is-the-old-frame rule as [`Self::time_in_force`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gtd_expiry: Option<i64>,
    /// The quantity the venue SHOWS of this order (an iceberg's visible slice, `vike_model::OrderRequest::display_quantity`);
    /// `None` shows all of it. Above zero, below `qty`, on a limit order only (`vike_model::display_refusal`).
    /// Requires [`crate::proto::FEATURE_ORDER_DISPLAY`], refused client-side otherwise: an older node drops the key
    /// and rests the whole quantity in view. Absent when `None`, so an order that names none is the frame from before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_quantity: Option<f64>,
    /// WHEN the node may send the order (`vike_model::OrderRequest::release`): `None` sends it now. Requires
    /// [`crate::proto::FEATURE_ORDER_RELEASE`], refused client-side otherwise: an older node drops the key and sends
    /// the order AT ONCE behind a normal Ack. Absent when `None`, as [`Self::display_quantity`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<OrderReleaseWire>,
    /// A trailing order's TRAIL, the distance in price (never a percent) its stop keeps from the best price
    /// since it activated (`vike_model::OrderRequest::trail`); a `"trailing"` or `"trailing_limit"` order
    /// needs one above zero, and every other kind is refused one by the node (its paper book would lower
    /// it as a trailing stop). The activation rides [`Self::trigger_price`]. Requires
    /// [`crate::proto::FEATURE_ORDER_TRAILING`], refused client-side otherwise. Absent when `None`, as
    /// [`Self::display_quantity`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<f64>,
    /// A `"trailing_limit"` order's LIMIT OFFSET, in price, zero or more: its limit is the stop less it
    /// for a sell, plus it for a buy (`vike_model::OrderRequest::limit_offset`); no other kind takes one.
    /// Same gate and the same absent-is-the-old-frame rule as [`Self::trail`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_offset: Option<f64>,
    /// A `"pegged"` order's PEG (`vike_model::OrderRequest::peg`): it needs one, and every other kind is refused
    /// one by the node. Requires [`crate::proto::FEATURE_ORDER_PEG`], refused client-side otherwise. Absent when
    /// `None`, as [`Self::display_quantity`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peg: Option<OrderPegWire>,
    /// A LIMIT order's ATTRIBUTES (`vike_model::OrderRequest::attributes`: all-or-none, sweep-to-fill, a
    /// discretionary amount), judged by `vike_model::attributes_refusal` at the node's door. Requires
    /// [`crate::proto::FEATURE_ORDER_ATTRIBUTES`], refused client-side otherwise: an older node drops the key and
    /// sends a plain limit. Absent when `None`, as [`Self::display_quantity`]. The model's `OrderAttributes` itself: ⚠ its
    /// `deny_unknown_fields` is load-bearing, a member added later is REFUSED by a node that does not know it, never
    /// dropped so the order goes out without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<OrderAttributes>,
    /// A trailing order's distance as a PERCENT of the extreme, above 0 and below 100, in place of [`Self::trail`]
    /// (exactly one of the two, `vike_model::order_terms_refusal`). Same gate and the same absent-is-the-old-frame
    /// rule as [`Self::attributes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_percent: Option<f64>,
    /// A `"stop"` or `"stop_limit"` order's ADJUSTMENT (`vike_model::OrderRequest::adjust`, judged by
    /// `vike_model::stop_adjust_refusal`). Same gate and the same absent-is-the-old-frame rule as [`Self::attributes`], and
    /// the model's `OrderStopAdjust` itself, `deny_unknown_fields` as `OrderAttributes` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjust: Option<OrderStopAdjust>,
}

/// A pegged order's peg: `vike_model::OrderPeg` with its `anchor` as the WORD. The node reads `anchor` through
/// `vike_model::OrderPegAnchor::parse` (exactly `best`, `mid`, `market` or `primary`) and refuses any other word, naming
/// the four.
///
/// ⚠ **`deny_unknown_fields` is load-bearing**, as on the model's: a term added beside these later is REFUSED by a
/// node that does not know it, never dropped so the order goes out without it. A new term rides a new
/// `Welcome.features` word of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderPegWire {
    /// The price of the touch the venue keeps the order at: `best` (its own side), `mid`, `market` (the far side)
    /// or `primary` (its own side of the national best bid and offer).
    pub anchor: String,
    /// The distance from the anchor, in price: positive is aggressive (a buy pays more), negative passive.
    pub offset: f64,
}

/// WHEN a held order may be released, a mirror of `vike_model::OrderRelease` that the node's door copies across: a
/// moment, and optionally a price condition (the owner's
/// conditional order: time AND price). Every condition it states must hold.
///
/// ⚠ **`deny_unknown_fields` is load-bearing**, as on the model's: a condition added beside these later is REFUSED
/// by a node that does not know it, never dropped so the order releases on time alone. A new condition rides a new
/// `Welcome.features` word of its own; the price condition shipped WITH [`crate::proto::FEATURE_ORDER_RELEASE`], so
/// that word covers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderReleaseWire {
    /// Not before this instant, epoch milliseconds UTC.
    pub at: i64,
    /// ...and not before the price crosses this level. Absent when `None`, so a time-only release is the frame
    /// without the condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<OrderReleasePriceWire>,
    /// The instrument whose price [`Self::price`] reads (`vike_model::OrderReleasePrice::symbol`), on the order's
    /// venue and account; `None`, and an absent key, is the ORDER's own symbol, so a condition on it is the frame
    /// without the field. Meaningful beside a price condition only: the node refuses one without it, and one its
    /// engine does not trade (it could never read that price, so the order would never go). It shipped with
    /// [`crate::proto::FEATURE_ORDER_RELEASE`] before any node advertised the word, so that word covers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The VENUE whose price [`Self::price`] reads when it is not the order's own (`vike_model::OrderReleasePrice::venue`,
    /// a roster id, exact and lower case): a price source only, never traded. Needs a price condition and a
    /// [`Self::symbol`] (the venues spell one instrument differently). Requires
    /// [`crate::proto::FEATURE_ORDER_ATTRIBUTES`], refused client-side otherwise (an older node's `deny_unknown_fields`
    /// refuses the key anyway). Absent when `None`. ⚠ Only a SUBMIT carries it: a held row's reading is
    /// [`OrderHeldViewWire::release_venue`], beside this type, never inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
}

/// The price condition of a [`OrderReleaseWire`], on the instrument [`OrderReleaseWire::symbol`] names (absent: the order's
/// own): the price is at or ABOVE the level, or at or BELOW it. JSON
/// `{"above": 100.0}` / `{"below": 100.0}`; the operator's word is `above:100` / `below:100`, or
/// `above:ETHUSDT:3500` for another instrument (`vike_client::verbs::release_price_from_word`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderReleasePriceWire {
    /// Released once the price is at or above the level.
    Above(f64),
    /// Released once the price is at or below the level.
    Below(f64),
}

impl OrderRequestWire {
    /// **The price this order names that can price it**: [`Self::price`], unless it is a MARKET order, which fills at
    /// the market whatever it names (10 BTC "at 1" is not an order worth 10). The core's `RiskGate::check_inner` reads
    /// its `limit_price` the same way. The word is read as `vike_model::OrderType::parse` reads it (trimmed, ASCII
    /// case-insensitive), and `crates/vike-client/src/verbs_tests.rs`'s
    /// `the_wire_market_word_reads_as_the_nodes_parser_reads_it` holds the two together.
    #[must_use]
    pub fn limit_price(&self) -> Option<f64> {
        if self.order_type.trim().eq_ignore_ascii_case("market") { None } else { self.price }
    }

    /// **The price of its own this order is sized at**: [`Self::limit_price`], else the trigger (a stop names its
    /// trigger and no price, and a trigger on a market order still sizes it: `fixtures/r5/risk.json` case 133).
    /// `None`: the node judges it at the mark. `RiskGate::check_inner`'s `own_price` in the wire's words, so the
    /// node's door and the client preview cannot size one order at two prices. A DISCRETIONARY limit is sized at its
    /// worst price, L + d for a buy and L − d for a sell, as that gate judges it (decision 0267).
    #[must_use]
    pub fn own_price(&self) -> Option<f64> {
        let d = self.attributes.and_then(|a| a.discretionary_amount).unwrap_or(0.0);
        self.limit_price().map(|p| if self.side > 0 { p + d } else { p - d }).or(self.trigger_price)
    }
}

/// One TP/SL BRACKET — mirrors `vike_model::OrderBracket` field for field: an entry (market when
/// `entry_price` is `None`, else limit) plus a stop-loss and a take-profit, which the core holds
/// until the entry fills and then sends as reduce-only OCO siblings (`vike_model::build_bracket`).
///
/// ⚠ **Its one order client id is the optional group STEM** ([`Self::order_client_id`]); a bracket naming none
/// has its three ids minted by the core. A bracket that names NO account is
/// accepted only on the default account of a venue with exactly ONE engine; one that names an
/// account ([`Self::account`]) goes to that account's engine. Either way the engine must trade the
/// bracket's symbol, and on binance and aster must be a PERP; the node also refuses a side other
/// than ±1, non-finite or non-positive sizes/prices and exits on the wrong side, all before its
/// `Ack` (`vike-tradehub`'s `account_refusal` and `bracket_refusal`). Without a stem the core mints
/// the three ids after the `Ack`; the entry id reaches the client as each held exit's `parent_order_id`.
///
/// ⚠ **`deny_unknown_fields` is load-bearing**: an older node must ERROR on a key it does not know
/// rather than drop it and route to the default book (that is how [`Self::account`] stays safe
/// against a node from before it). Never relax it to add a field; a new field rides a new
/// `Welcome.features` word ([`crate::proto::FEATURE_BRACKET_ACCOUNT`] for the account,
/// [`crate::proto::FEATURE_BRACKET_STEM`] for the stem).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderBracketWire {
    pub venue: String,
    pub symbol: String,
    /// Entry side: +1 long / -1 short. The two exits are the opposite side.
    pub side: i32,
    pub qty: f64,
    /// `None` = a market entry, `Some(px)` = a limit entry. There is no stop entry.
    ///
    /// ⚠ **Required on decode**: an omitted key is refused (serde would default it to `None`,
    /// turning a forgotten limit into a market order); an explicit `null` is the only market
    /// spelling. ⚠ **A NON-FINITE price is written as `null`, i.e. a MARKET entry; the guard is
    /// the SENDER's** (a failing serializer would end the whole control link in
    /// `crate::tradehub_control`). `crate::wire::wire_tests`'
    /// `a_non_finite_entry_price_round_trips_to_a_market_entry` pins it.
    #[serde(deserialize_with = "Option::<f64>::deserialize")]
    pub entry_price: Option<f64>,
    /// The stop-loss trigger price.
    pub stop_loss: f64,
    /// The take-profit limit price.
    pub take_profit: f64,
    /// WHICH ACCOUNT of `venue`; `None` = the sender named none ([`OrderRequestWire::account`]'s three
    /// wire states: absent = named nothing, `"DEFAULT"` = the unlabelled account, a label = that one).
    /// Requires [`crate::proto::FEATURE_BRACKET_ACCOUNT`] (refused client-side otherwise).
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing**: a bracket naming no account is BYTE-IDENTICAL to
    /// the pre-field frame, so it still decodes on a node from before the field (whose
    /// `deny_unknown_fields` would refuse the key). And `default` is what lets this node read that old
    /// frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The sender's group STEM (decision 0270: a v2 order client id through its unique part), from which the node
    /// derives the three leg ids, so a bracket resent after an unclear reply names the same legs and is refused
    /// whole; `None` = the node mints the ids. Requires [`crate::proto::FEATURE_BRACKET_STEM`] (refused client-side
    /// otherwise), and the same `skip_serializing_if` rule as [`Self::account`]: a bracket naming none is the
    /// pre-field frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id: Option<String>,
}

/// One leg of a [`OrderListWire`], a field-for-field mirror of `vike_model::OrderListLeg`: an order, and its links to
/// the list's other legs BY INDEX. The node reads the words as a `Submit`'s (`OrderType::parse`, `TimeInForce::parse`)
/// and refuses what `vike_model::build_order_list` refuses, naming the leg.
///
/// ⚠ `deny_unknown_fields`, as on [`OrderListWire`]: a field a later build adds to a leg is REFUSED by a node that does
/// not know it, never dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderListLegWire {
    /// +1 buy / -1 sell.
    pub side: i32,
    pub qty: f64,
    /// [`OrderRequestWire::order_type`]'s words.
    pub order_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<f64>,
    /// A trailing leg's TRAIL, the distance in price (`vike_model::OrderListLeg::trail`; its activation rides
    /// [`Self::trigger_price`]), refused by the node on every other kind. Absent when `None`, so a list with no
    /// trailing leg is the frame from before. It shipped with [`crate::proto::FEATURE_ORDER_LIST`] before any
    /// released node advertised the word, so that word covers it, as [`Self::order_client_id`]; a node built before
    /// it refuses the key on decode, never drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reduce_only: bool,
    /// [`OrderRequestWire::time_in_force`]'s words; `None` = `gtc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<String>,
    /// OTO: the EARLIER leg whose fill arms this one, held off the venue until then; `None` = live at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// `Oco` or `Ouo`, set exactly when [`Self::linked`] is not empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contingency: Option<ContingencyType>,
    /// The legs this leg's fill cancels or reduces; each names this leg back with the same type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked: Vec<usize>,
    /// This leg's order client id, pre-minted by the client as [`OrderRequestWire::order_client_id`] is, so a resend
    /// after an unclear reply names the same ids and the node refuses it WHOLE instead of placing a second list. The
    /// node refuses a list whose legs do not all name one. Absent when `None`. It shipped with
    /// [`crate::proto::FEATURE_ORDER_LIST`] before any node advertised the word, so that word covers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id: Option<String>,
}

/// One ORDER LIST, a mirror of `vike_model::OrderList`: linked orders on ONE symbol of ONE account (decision 0254),
/// an OCO pair, an OTO chain, an OUO group or a ladder. Requires [`crate::proto::FEATURE_ORDER_LIST`].
///
/// ⚠ **Every leg names its order client id** ([`OrderListLegWire::order_client_id`]), minted by the client before
/// the preview as a submit's is, and the node's `Ack` echoes them (comma-joined): a retry with the same ids is refused
/// whole. A list that names no account goes only where the venue has one account
/// ([`CommandWire::names_no_account`]); one that names an account goes to that account's engine, which must trade the
/// list's symbol (`vike-tradehub`'s `order_list_refusal`).
///
/// ⚠ **`deny_unknown_fields` is load-bearing**, as on [`OrderBracketWire`]: a field added later rides a new
/// `Welcome.features` word and is refused, never dropped, by a node from before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderListWire {
    pub venue: String,
    pub symbol: String,
    pub legs: Vec<OrderListLegWire>,
    /// WHICH ACCOUNT of `venue`, [`OrderRequestWire::account`]'s three wire states. It ships with the word, so it owes
    /// nothing more; absent when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl OrderListWire {
    /// The one wording of a list for a preview line (the CLI's, Telegram's): `ORDER LIST <venue> <symbol>
    /// account=<label|->: leg 0 buy 3 market; leg 1 sell 3 stop trigger=90 [reduce-only] after 0 OUO 2; ...`.
    #[must_use]
    pub fn say(&self) -> String {
        let legs: Vec<String> = self
            .legs
            .iter()
            .enumerate()
            .map(|(i, leg)| {
                let mut s = format!(
                    "leg {i} {} {} {}",
                    if leg.side > 0 { "buy" } else { "sell" },
                    leg.qty,
                    leg.order_type
                );
                if let Some(p) = leg.price {
                    s.push_str(&format!(" @ {p}"));
                }
                if let Some(t) = leg.trigger_price {
                    s.push_str(&format!(" trigger={t}"));
                }
                if let Some(t) = leg.trail {
                    s.push_str(&format!(" trail={t}"));
                }
                if let Some(tif) = &leg.time_in_force {
                    s.push_str(&format!(" [{tif}]"));
                }
                if leg.reduce_only {
                    s.push_str(" [reduce-only]");
                }
                if let Some(p) = leg.parent {
                    s.push_str(&format!(" after {p}"));
                }
                if let Some(c) = leg.contingency {
                    let linked: Vec<String> = leg.linked.iter().map(usize::to_string).collect();
                    s.push_str(&format!(" {c:?} {}", linked.join(",")).to_uppercase());
                }
                if let Some(order_client_id) = &leg.order_client_id {
                    s.push_str(&format!(" order_client_id={order_client_id}"));
                }
                s
            })
            .collect();
        format!(
            "ORDER LIST {} {} account={}: {}",
            self.venue,
            self.symbol,
            self.account.as_deref().unwrap_or("-"),
            legs.join("; ")
        )
    }
}

/// One COMBO, a mirror of `vike_model::OrderCombo`: two legs or more traded as ONE order on ONE account (decision 0254),
/// priced by its signed NET ([`Self::net_limit`]), by a limit per leg ([`OrderComboLeg::limit`]), or at market. Requires
/// [`crate::proto::FEATURE_COMBO`].
///
/// ⚠ **The combo names its ONE order client id** ([`Self::order_client_id`]), minted by the client before the preview
/// as a submit's is, and the node's `Ack` echoes it: a retry under the same id is refused whole, never placed twice.
/// A combo that names no account goes only where the venue has one account ([`CommandWire::names_no_account`]); one
/// that names an account goes to that account's engine. Only a venue whose row takes a combo places it (deribit, by
/// its net), and only a paper book fills one priced by its legs; every other engine rejects it after the `Ack`, by name.
///
/// ⚠ **`deny_unknown_fields` is load-bearing**, as on [`OrderBracketWire`]: a field added later rides a new
/// `Welcome.features` word and is refused, never dropped, by a node from before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderComboWire {
    pub venue: String,
    /// +1 buys the combo, -1 sells it (every leg flips with it).
    pub side: i32,
    /// Combo units, finite and above zero.
    pub qty: f64,
    /// Two legs or more, in the venue's creation order: each the model's own [`OrderComboLeg`] (an instrument, its SIGNED
    /// ratio, `side × ratio > 0` buying the leg, and on a combo priced by its legs the leg's own limit), whose
    /// `deny_unknown_fields` refuses a key a later build adds to a leg, never drops it. Every leg names a limit or none
    /// does, never beside [`Self::net_limit`] (the node refuses either mix in `vike_model::OrderCombo::validate`'s words).
    pub legs: Vec<OrderComboLeg>,
    /// The NET limit per combo unit, SIGNED (`Σ ratio × price`: a debit above zero, a credit below, zero legal); `None`
    /// with no leg limit is a combo MARKET order.
    ///
    /// ⚠ **Required on decode**, as [`OrderBracketWire::entry_price`]: an omitted key is refused (serde would default it
    /// to `None`, turning a forgotten limit into a market combo); an explicit `null` is the only spelling of none. A
    /// NON-FINITE net is written as `null`; the guard is the SENDER's.
    #[serde(deserialize_with = "Option::<f64>::deserialize")]
    pub net_limit: Option<f64>,
    /// WHICH ACCOUNT of `venue`, [`OrderRequestWire::account`]'s three wire states. It ships with the word, so it owes
    /// nothing more; absent when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The combo's LIFETIME, [`OrderRequestWire::time_in_force`]'s words (`vike_model::TimeInForce::parse` reads them);
    /// absent = `gtc`. Which lifetimes a book or a venue takes is theirs to say (a paper book and the simulator hold a
    /// combo `gtc` only; deribit takes `ioc` and `fok` natively). It ships with the word; absent when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<String>,
    /// The combo's ONE order client id, pre-minted by the client as [`OrderRequestWire::order_client_id`] is, so a
    /// resend after an unclear reply names the same id and the node refuses it WHOLE instead of placing a second combo.
    /// The node refuses a combo that names none. It ships with the word; absent when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id: Option<String>,
}

impl OrderComboWire {
    /// The one wording of a combo for a preview line (the CLI's, Telegram's): `COMBO <venue> account=<label|-> buy 2:
    /// +1 A @ 0.1, -1 B @ 0.05 (by its legs) [ioc] order_client_id=<id>`, the price `net <n>`, `by its legs` or `market`, the
    /// lifetime and the id only when named.
    #[must_use]
    pub fn say(&self) -> String {
        let legs: Vec<String> = self
            .legs
            .iter()
            .map(|leg| match leg.limit {
                Some(p) => format!("{:+} {} @ {p}", leg.ratio, leg.symbol),
                None => format!("{:+} {}", leg.ratio, leg.symbol),
            })
            .collect();
        let price = match self.net_limit {
            Some(net) => format!("net {net}"),
            None if self.legs.iter().any(|leg| leg.limit.is_some()) => "by its legs".to_string(),
            None => "market".to_string(),
        };
        let mut said = format!(
            "COMBO {} account={} {} {}: {} ({price})",
            self.venue,
            self.account.as_deref().unwrap_or("-"),
            if self.side > 0 { "buy" } else { "sell" },
            self.qty,
            legs.join(", ")
        );
        if let Some(tif) = &self.time_in_force {
            said.push_str(&format!(" [{tif}]"));
        }
        if let Some(order_client_id) = &self.order_client_id {
            said.push_str(&format!(" order_client_id={order_client_id}"));
        }
        said
    }
}

/// The command vocabulary a [`Scope::Write`](vike_hub_protocol::auth::Scope) client may issue — a mirror of
/// the SESSION-relevant verbs in `vike_execution::{Command, OrderIntent}` (orders, brackets, cancels,
/// flatten, trading state, strategy params/mounts, settings writes, and the operator's
/// `ReconcileConfirm` of one held reconcile divergence). NOT the rest of the reconcile/journal
/// surface (`ReconcileReports`, …): that is core-internal plumbing and never a
/// remote client's business. A server lowers each variant into the core's
/// `Command`/`OrderIntent` at its edge.
///
/// ⚠ `Debug` stays DERIVED: no variant may carry a credential value (0065 §4.1). The account verbs
/// that do are [`AccountRequestWire`], outside this enum, with a redacting `Debug`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "a command is built once per operator act and never queued in bulk, so the submit's size (the \
              attributes wave's terms took it past the lint) costs nothing; boxing it would move every call site \
              of the one wire type every surface builds"
)]
pub enum CommandWire {
    /// Submit one order (`OrderIntent::Submit`).
    Submit(OrderRequestWire),
    /// Submit one TP/SL BRACKET (`OrderIntent::Bracket`). Naming no account reaches only the default
    /// book of a one-engine venue — see [`OrderBracketWire`]. Requires
    /// [`crate::proto::FEATURE_BRACKET`], and [`crate::proto::FEATURE_BRACKET_ACCOUNT`] when it names an
    /// account (refused client-side otherwise).
    Bracket(OrderBracketWire),
    /// Submit one ORDER LIST (`OrderIntent::OrderList`): an OCO, OTO or OUO group or a ladder on one account and one
    /// symbol — see [`OrderListWire`]. Requires [`crate::proto::FEATURE_ORDER_LIST`] (refused client-side otherwise).
    OrderList(OrderListWire),
    /// Submit one COMBO (`OrderIntent::Combo`): two legs or more traded as one order on one account — see
    /// [`OrderComboWire`]. Requires [`crate::proto::FEATURE_COMBO`] (refused client-side otherwise).
    Combo(OrderComboWire),
    /// Cancel one order by order-client-id (`OrderIntent::Cancel`).
    Cancel(String),
    /// Modify one resting order's qty, price and/or trigger price (`OrderIntent::Modify`). Where `new_price` and
    /// `new_trigger_price` land on each kind is `vike_model::modified_prices` (an explicit trigger wins on a stop).
    Modify {
        order_client_id: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        /// The trigger price of a trigger order (a stop's, a stop-limit's, a take-profit's). Requires
        /// [`crate::proto::FEATURE_ORDER_MODIFY_TRIGGER`], refused client-side otherwise: an older node drops the key
        /// and changes the rest, or nothing, behind a normal Ack. Absent when `None`, so every modify that names no
        /// trigger is the frame from before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_trigger_price: Option<f64>,
    },
    /// Cancel every live order, optionally scoped to a venue/symbol (`OrderIntent::MassCancel`).
    /// `None`/`None` = all engines + all books.
    ///
    /// With a `venue` and no `account`, the node accepts it only where that venue has ONE account (decision
    /// 0254, [`Self::names_no_account`]). A named one cancels only that book, on a node advertising
    /// `crate::proto::FEATURE_ACCOUNT_SCOPED_REDUCE` (it refuses an unknown account, or one with no `venue`,
    /// before the Ack); against an older node, which would cancel every account, the frame is refused
    /// client-side.
    MassCancel {
        venue: Option<String>,
        symbol: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// Close the `(venue, symbol)` net position with a reduce-only market order
    /// (`OrderIntent::Flatten`); no-op when flat. An absent `account` closes it on every account of
    /// `venue`; a named one narrows it, under [`CommandWire::MassCancel`]'s capability/refusals.
    Flatten {
        venue: String,
        symbol: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// PANIC BUTTON — cancel every live order, then flatten every position
    /// (`OrderIntent::MarketExit`). `venue: None` = every engine.
    ///
    /// ⚠ **An absent `account` FANS OUT; it does not refuse** — the RISK-DIRECTION LAW
    /// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §4.5):
    /// *"A risk-REDUCING venue verb that names no account fans out to EVERY account of that venue.
    /// A risk-INCREASING one refuses."* So this, `MassCancel` and `SetTradingState` reach every
    /// account, while a `Submit` naming none refuses.
    ///
    /// A named account narrows both legs and the core-held protection to that book, under
    /// `MassCancel`'s capability and refusals (`crate::tradehub_control::required_feature`).
    MarketExit {
        venue: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// Set the account trading state / kill switch (`Command::SetTradingState`).
    SetTradingState(TradingState),
    /// LIVE PARAMETER update for a mounted strategy (`Command::UpdateParams` /
    /// `vike_execution::ParamsUpdate`). `(venue, symbol, interval)` names the series EXACTLY and
    /// `mount_id` which mount on it. A key or id naming no live mount, an id on a different series,
    /// and an unaddressed update on a series two mounts share are each REFUSED by the core with a
    /// recent-events note: the core never picks the first.
    ///
    /// `params` is the core's own `vike_model::StrategyParams` serde JSON (e.g.
    /// `{"SpreadMaker": { … }}`) as a [`serde_json::Value`] — the ONE departure from the mirror
    /// idiom: the union grows with every strategy and is already a persisted journal schema, so the
    /// wire DELEGATES. The daemon (`lower_command`) refuses an undecodable payload with
    /// `Response::Error`. Requires [`crate::proto::FEATURE_STRATEGY_VERBS`].
    UpdateParams {
        /// Target mount venue (exact match).
        venue: String,
        /// Target mount symbol (exact match).
        symbol: String,
        /// Target mount bar interval (exact match, e.g. `"1m"`).
        interval: String,
        /// WHICH mount on the series: a `MountViewWire::mount_id` (matched as the core stores it,
        /// so `maker-a` finds `maker_a`). `None`: delivered when exactly one mount sits on the
        /// series, refused when several share it.
        ///
        /// ⚠ Gated by [`crate::proto::FEATURE_PARAMS_BY_MOUNT`], NOT `strategy-verbs`: an older
        /// node drops the key and retunes the FIRST mount behind a normal Ack, so the client
        /// refuses it there ([`crate::tradehub_control`]'s `required_feature`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mount_id: Option<String>,
        /// The typed `vike_model::StrategyParams` payload, in the core's own serde JSON form.
        params: serde_json::Value,
    },
    /// RUNTIME strategy MOUNT (`Command::MountStrategy` / `vike_execution::MountSpec`) without a
    /// restart. The source is the profile `[strategy]` vocabulary: a registry `name` XOR a `rhai`
    /// script path (docs/decisions/0024-rhai-strategies-live.md), plus `params` as JSON. The daemon
    /// validates at its edge like a profile load (`Response::Error`); a refusal the core raises
    /// later (duplicate id, unknown venue) lands in recent-events. Requires
    /// [`crate::proto::FEATURE_MOUNT_VERBS`].
    MountStrategy {
        /// Target venue — must name an engine the node's core already runs.
        venue: String,
        /// WHICH ACCOUNT of `venue` the mount trades and reads ([`OrderRequestWire::account`]'s
        /// three states). Requires [`crate::proto::FEATURE_MOUNT_ACCOUNT`]; set against a node
        /// without it, the client refuses locally.
        ///
        /// ⚠ A misrouted MOUNT is every order the strategy places, sized against the wrong book —
        /// hence its own capability. An unknown account is a `Response::Error` BEFORE the Ack
        /// (`vike-tradehub`'s `account_refusal`), not a recent-events refusal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
        /// The mount's own symbol.
        symbol: String,
        /// The mount's bar-series interval (e.g. `"1m"`).
        interval: String,
        /// Optional explicit mount identity; `None` derives `{venue}__{symbol}__{interval}`.
        controller_id: Option<String>,
        /// The strategy FILE: its path under the NODE's `<project>/strategies/` folder (a `.rs`
        /// or a `.rhai` file). Absent from the frame while `None`; a node from before the field
        /// reads the frame as naming no strategy and refuses it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        /// ⚠ RETIRED: a registry strategy name, refused by the node with the fix (name the file).
        name: Option<String>,
        /// ⚠ RETIRED: a Rhai script path, refused by the node with the fix (name the file).
        rhai: Option<String>,
        /// The `[strategy.params]` table as a JSON object.
        params: serde_json::Value,
    },
    /// RUNTIME strategy UNMOUNT (`Command::UnmountStrategy`) by mount id. The core CANCELS the
    /// mount's attributed live orders and saves its durable state; positions are NOT flattened
    /// (`Flatten` does that). An unknown id lands in recent-events. Requires
    /// [`crate::proto::FEATURE_MOUNT_VERBS`].
    UnmountStrategy {
        /// The mount id to remove.
        controller_id: String,
    },
    /// SETTINGS write: set ONE ROW of the node's settings database (`<settings>/db/vike.db`) by its
    /// KEY (`docs/decisions/0086`). The node validates the row with its `vike-config` loader inside
    /// one transaction before commit, so a write cannot leave a store the next boot refuses.
    /// STRINGLY, like `SettingsShowRowWire`: the daemon is the typing authority.
    ///
    /// `Response::SettingsWritten`'s `restart_required` is the node's per-key call: `false` only
    /// for a hot-safe key it applied live (never `policy.*`). Requires
    /// [`crate::proto::FEATURE_SETTINGS_WRITE`] (`crate::tradehub_control::set_setting`).
    ///
    /// ⚠ **`file` and `confirm` are FILE-ERA fields the node ignores** (0086 point 7) and decode
    /// as absent, yet every client KEEPS SENDING `file` (the key's first segment) and
    /// `confirm: None` until they are deleted: the released v0.1.35 daemon REQUIRES `file`.
    SetSetting {
        /// IGNORED by the node — see the variant doc. A client sends the key's section word.
        #[serde(default)]
        file: String,
        /// The FULL dotted key as `SettingsShowRowWire::key` renders it
        /// (`"policy.max_notional_per_order"`): first segment = section.
        key: String,
        /// The new value as TEXT: parsed as a TOML value when it is one, else a string; then the
        /// loader validates it.
        value: String,
        /// IGNORED by the node (0086 point 7); every client sends `None`.
        #[serde(default)]
        confirm: Option<String>,
    },
    /// CONFIRM one HELD reconcile divergence by its id (`Command::ConfirmReconcile`): the core folds
    /// what the divergence proposed into the account's book - the venue's position for
    /// `PositionOnlyExternal`/`PositionDrift`, the missed fill for `MissingFill`, the venue's cash
    /// figure for `BalanceDrift`, and for `OrphanLocalPosition` the DROP of the local position the
    /// venue never reported (one closing fill at the position's own average price: zero PnL).
    /// `id` is a [`ReconcileAlertViewWire::id`]; an unknown, already-confirmed or since-cleared id is a
    /// note in recent-events, never an error (and a refusal: nothing folds). The id is the
    /// divergence's identity digest, so it names the same divergence after a node restart.
    /// Names no venue and no account: the held row carries the book it belongs to. Requires
    /// [`crate::proto::FEATURE_RECONCILE_VERBS`].
    ReconcileConfirm {
        /// The held divergence's id (`ReconcileAlertViewWire::id`).
        id: u64,
    },
    /// CANCEL, at the venue, the ONE order a held `UnknownOrder` row names (`Command::CancelReconcileOrder`,
    /// decision 0196 rule (b)): the core sends a single cancel for the client id the row carries,
    /// through that account's own client. `demo`-tier accounts only (any other tier, a row of
    /// another kind, a row with no id the venue can cancel by, or an id the node no longer holds is a
    /// refusal note in recent-events and a WARN, nothing sent, still an `Ack`: the verdict is the
    /// core's). It folds nothing and removes no row; the row clears at the next reconcile pass that no
    /// longer raises it. Operator only: the CLI's `trade reconcile cancel <id>` is its one constructor,
    /// and no MCP tool, GUI call or strategy builds it. Names no venue and no account: the held row
    /// carries the book it belongs to. Requires [`crate::proto::FEATURE_RECONCILE_CANCEL`], refused
    /// client-side otherwise: a node that predates it cannot decode the variant.
    ReconcileCancel {
        /// The held row's id (`ReconcileAlertViewWire::id`).
        id: u64,
    },
    /// RESUME ONE VENUE's link dead-man halt early (`Command::ResumeVenue`, decision 0180). The link
    /// dead-man halts the engines of the venue whose link died and nothing else; `SetTradingState`
    /// is node-wide, so a `resume` also clears every other venue's halt and un-latches the drawdown
    /// latch. This clears ONLY `venue`'s, to the states its engines held before the halt.
    ///
    /// The node REFUSES (a note in recent-events and a WARN in its log, not an error reply: the
    /// command still reports accepted, because the verdict is the core's) while one of the venue's
    /// links is still down, unless `force`. A forced resume starts the venue's windows over, so a
    /// link that stays down trips again after a fresh grace. An unknown `venue` is refused at the
    /// node's edge before the `Ack` ([`CommandWire::addressed_venue`]). Requires
    /// [`crate::proto::FEATURE_VENUE_RESUME`], refused client-side otherwise: a node that predates it
    /// cannot decode the variant.
    ResumeVenue {
        /// The exchange whose link-dead-man halt to release (a venue id, not a route key).
        venue: String,
        /// Resume although a link of the venue is still down.
        #[serde(default)]
        force: bool,
    },
    /// Set the trading state of ONE ACCOUNT's engine and no other (`Command::SetAccountTradingState`):
    /// `SetTradingState` is node-wide, so a `resume` also loosens every engine a latch or an operator halt
    /// holds. The STRICTER state wins: the node's own floor (the drawdown latch's `Reducing`, a node-wide
    /// `halt`, a fault) is never lowered by this verb, a looser state lands at the floor and the node says
    /// so in recent-events (still an `Ack`: the verdict is the core's); only a node-wide `Active` clears it.
    /// An engine the link dead-man holds `Halted` stays so (`trade venue resume` is its way out).
    ///
    /// `venue` and `account` name the book as every account-scoped verb does (`account` is
    /// `parse_wire_account`'s: `DEFAULT` or a label). An account this node does not run is refused at the
    /// node's edge before the `Ack` (`account_refusal`), naming the accounts it does run. An ABSENT account is a
    /// bare venue, taken only where the venue has one account, running or not ([`Self::names_no_account`], decision 0254), which
    /// owes [`crate::proto::FEATURE_ONE_ACCOUNT_VENUE`]. Requires
    /// [`crate::proto::FEATURE_ACCOUNT_TRADING_STATE`], refused client-side otherwise: a node that predates
    /// it cannot decode the variant.
    SetAccountTradingState {
        venue: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
        state: TradingState,
    },
    /// GIVE BACK one symbol the account's engine admitted on demand (`SymbolAdmission::release`, decision 0205):
    /// the engine takes it out and its slot (one of sixteen per engine) is free for the next symbol, the
    /// gatewayhub's reconcile client stops reading it, and the node forgets the facts, the price lane and the idle
    /// stamps it kept for it. Only a symbol the engine is QUIET in is given back: a position, a live order, a
    /// conditional or held exit in it, or an order created in the last minute refuses it (the node answers a
    /// published one before the `Ack`, naming it; the engine's own verdict is a note in recent-events and a WARN,
    /// still an `Ack`: it is the core's). Writes NOTHING at the venue, so it is allowed on every tier and in
    /// every trading state; the account's leverage for the symbol stays what the venue holds.
    ///
    /// `venue` and `account` name the book as every account-scoped verb does (`account` is `parse_wire_account`'s:
    /// `DEFAULT` or a label; absent is a bare venue, as on [`CommandWire::SetAccountTradingState`]); `symbol` is
    /// the engine's spelling or any case of it (the node sends the engine's own). Requires
    /// [`crate::proto::FEATURE_UNADMIT_SYMBOL`], refused client-side otherwise: a node that predates it cannot
    /// decode the variant.
    UnadmitSymbol {
        venue: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
        symbol: String,
    },
    /// CLEAR one mount's per-mount BUDGET LATCH (`Command::UnlatchMount`, decision 0239): a mount that breached its
    /// `MountBudget` stays liquidate-only until this. The node refuses before the `Ack` a mount it does not run, one
    /// that is not latched (the published mount row says) and one that still holds a position; the core refuses the
    /// rest in a note and a WARN, still an `Ack` (it also sees the mount's live orders and armed conditionals,
    /// which the published rows do not carry). An accepted un-latch clears the latch and re-arms the loss budget
    /// from the mount's CURRENT net realized PnL (the core says the new base in a recent-events note), leaves the
    /// account's trading state and every other mount alone, and writes nothing at a venue.
    ///
    /// `mount_id` is the id the mount rows publish ([`MountViewWire::mount_id`]); the core sanitizes it as it does
    /// for `UnmountStrategy` and the node sends the published spelling. It names no venue ([`Self::addressed_venue`]
    /// is `None`). Requires [`crate::proto::FEATURE_MOUNT_UNLATCH`], refused client-side otherwise: a node that
    /// predates it cannot decode the variant.
    UnlatchMount {
        /// The mount's id (`MountViewWire::mount_id`).
        mount_id: String,
    },
}

impl CommandWire {
    /// **The VENUE this command ADDRESSES**, or `None` when it addresses none — the ONE addressing
    /// key on this wire (the daemon's `lower_command` copies it onto
    /// `vike_model::OrderRequest::venue`; `vike_core`'s `CoreThread::route_of` resolves it).
    ///
    /// ⚠ `Some(v)` = "act on venue `v`", which a node with no engine for `v` must REFUSE
    /// (`vike-tradehub`'s `venue_refusal`; [`crate::proto::FEATURE_VENUE_ROUTING`]). `None` is no
    /// omission: an ORDER-scoped verb (`Cancel`/`Modify`, resolved by the core's `order_client_id_venue`
    /// map), an ACCOUNT-wide verb (`SetTradingState`, the `venue: None` arms of
    /// `MassCancel`/`MarketExit`), or a verb about no book (`UnmountStrategy`, `SetSetting`).
    /// ⚠ NO WILDCARD ARM: a new variant must be classified by hand, or a new order verb would
    /// silently become address-less.
    pub fn addressed_venue(&self) -> Option<&str> {
        match self {
            CommandWire::Submit(req) => Some(req.venue.as_str()),
            CommandWire::Bracket(b) => Some(b.venue.as_str()),
            CommandWire::OrderList(l) => Some(l.venue.as_str()),
            CommandWire::Combo(c) => Some(c.venue.as_str()),
            CommandWire::Flatten { venue, .. } => Some(venue.as_str()),
            CommandWire::UpdateParams { venue, .. } => Some(venue.as_str()),
            CommandWire::MountStrategy { venue, .. } => Some(venue.as_str()),
            // The exchange whose link-dead-man halt is released: a node with no engine for it
            // refuses before the `Ack` instead of Acking a no-op.
            CommandWire::ResumeVenue { venue, .. } => Some(venue.as_str()),
            // The exchange of the account whose state is set: a node with no engine for it refuses
            // before the `Ack`, in the venue gate's own words.
            CommandWire::SetAccountTradingState { venue, .. } => Some(venue.as_str()),
            // The exchange of the account whose symbol is given back: refused before the `Ack` for a venue the
            // node runs no engine for.
            CommandWire::UnadmitSymbol { venue, .. } => Some(venue.as_str()),
            // `Some(v)` = every engine of that exchange in the core (`CoreThread::exit_scope_engines`), which a
            // remote client reaches only for a venue of one account (`names_no_account`); `None` = every
            // engine, never refused.
            CommandWire::MassCancel { venue, .. } => venue.as_deref(),
            CommandWire::MarketExit { venue, .. } => venue.as_deref(),
            CommandWire::Cancel(_)
            | CommandWire::Modify { .. }
            | CommandWire::SetTradingState(_)
            | CommandWire::UnmountStrategy { .. }
            | CommandWire::SetSetting { .. }
            | CommandWire::ReconcileConfirm { .. }
            | CommandWire::ReconcileCancel { .. }
            // A mount is named by its id, which no venue prefixes.
            | CommandWire::UnlatchMount { .. } => None,
        }
    }

    /// **The venue and the subject of a command that names a venue and NO account**, the shape the one-account
    /// rule judges (decision 0254), or `None` for every other command: one that names its account, the node-wide
    /// panic button (no venue), an order named by id, and what opens and closes nothing. NO WILDCARD ARM, as above.
    pub fn names_no_account(&self) -> Option<(&str, &'static str)> {
        match self {
            CommandWire::Submit(req) if req.account.is_none() => Some((&req.venue, "the order")),
            CommandWire::Bracket(b) if b.account.is_none() => Some((&b.venue, "the bracket")),
            CommandWire::OrderList(l) if l.account.is_none() => Some((&l.venue, "the order list")),
            CommandWire::Combo(c) if c.account.is_none() => Some((&c.venue, "the combo")),
            CommandWire::MountStrategy { venue, account: None, .. } => Some((venue, "the mount")),
            CommandWire::Flatten { venue, account: None, .. } => Some((venue, "the flatten")),
            CommandWire::MassCancel { venue: Some(venue), account: None, .. } => {
                Some((venue, "the mass-cancel"))
            }
            CommandWire::MarketExit { venue: Some(venue), account: None } => {
                Some((venue, "the market exit"))
            }
            CommandWire::SetAccountTradingState { venue, account: None, .. } => {
                Some((venue, "the trading-state change"))
            }
            CommandWire::UnadmitSymbol { venue, account: None, .. } => {
                Some((venue, "the give-back"))
            }
            CommandWire::Submit(_)
            | CommandWire::Bracket(_)
            | CommandWire::OrderList(_)
            | CommandWire::Combo(_)
            | CommandWire::MountStrategy { .. }
            | CommandWire::Flatten { .. }
            | CommandWire::MassCancel { .. }
            | CommandWire::MarketExit { .. }
            | CommandWire::Cancel(_)
            | CommandWire::Modify { .. }
            | CommandWire::SetTradingState(_)
            | CommandWire::UpdateParams { .. }
            | CommandWire::UnmountStrategy { .. }
            | CommandWire::SetSetting { .. }
            | CommandWire::ReconcileConfirm { .. }
            | CommandWire::ReconcileCancel { .. }
            | CommandWire::ResumeVenue { .. }
            | CommandWire::SetAccountTradingState { .. }
            | CommandWire::UnadmitSymbol { .. }
            | CommandWire::UnlatchMount { .. } => None,
        }
    }
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;

#[path = "the_account_field_is_invisible_until_it_is_used.rs"]
#[cfg(test)]
mod the_account_field_is_invisible_until_it_is_used;
