//! The node wire value types: a STANDALONE serde mirror of the rendered core snapshot
//! ([`WireSnapshot`]) and of the order-write command vocabulary ([`WireCommand`]); the account
//! admin plane (decision 0065) lives in the private `account` child, re-exported here by name.
//!
//! # Why standalone mirrors (not the core types)
//!
//! These re-declare the field shapes of `vike_exec::{CoreSnapshot, OrderView, PositionView}`,
//! `vike_exec::{Command, OrderIntent}` and `vike_model::{OrderRequest, TradingState}` instead of
//! depending on `vike-core`/`vike-exec`/`vike-model`: the LIGHT thin-client crate stays at serde +
//! framing + auth crypto, and the mirror IS the schema, so a core refactor cannot silently change
//! it (a server projects `CoreSnapshot -> WireSnapshot` at its edge). The tradeoff: a field the GUI
//! must see is added here too, on purpose, versioned by [`crate::proto::NODE_PROTO_VERSION`].
//!
//! # The additive-field contract
//!
//! A later field carries `#[serde(default)]` (usually with `skip_serializing_if`): an old node's
//! frame still decodes and a client that says nothing sends the bytes it always sent. No
//! `NODE_PROTO_VERSION` bump: the version is in the signed auth message, so a bump fails the
//! handshake against every running node. ⚠ Absent/empty means "the node did not say"; where it
//! matters check the `Welcome.features` capability, never the emptiness.

use serde::{Deserialize, Serialize};

mod account;

pub use account::{
    AccountRequest, AccountVerb, WireAccountList, WireAccountRow, WireAccountWritten,
    WireCredentialsHeld, WireDirectory, WireDirectoryAccount, WireDirectoryVenue,
};

/// The account trading state — mirrors `vike_model::TradingState` (same variant names, so a JSON
/// round-trip matches the core enum's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireTradingState {
    /// Normal trading.
    Active,
    /// Only position-reducing orders allowed.
    Reducing,
    /// No new orders (kill switch).
    Halted,
}

/// One order — mirrors `vike_exec::OrderView`. `status` is the rendered `OrderStatus` string, so
/// this crate need not mirror that enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireOrderView {
    pub client_order_id: String,
    pub venue: String,
    /// WHICH ACCOUNT's book this order rests in. ⚠ Absent = the default account OR a node that
    /// predates the field. The tell: [`WireVenueBlock::tier`] (the account's tier word, which
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
    pub avg_fill_px: f64,
    /// The delay label of the data the order was priced off, as the node recorded it at pre-trade
    /// (decision 0140): `"real-time"`, `"delayed"` or `"frozen"` (`vike_model::DataDelay::word`).
    /// Absent = no label stated, or a node that predates the field. Shown, never a gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_delay: Option<String>,
    /// WHY the node refused the order (the `reason` of its `OrderRejected`/`OrderDenied`). Absent =
    /// not refused, no reason recorded, or a node that predates the field - additive, so an older
    /// reader ignores it and a newer reader of an older node sees `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// **How long this NODE PROCESS has known the order, in milliseconds, as of the frame** - stamped
    /// by the publisher (`crates/vike-tradehub/src/publish.rs`'s `NodeFacts::stamp`) off the fold, for
    /// an order that is not terminal. ⚠ NOT the order's age at the venue: the clock starts when this
    /// process first SAW the order, so after a restart an order that survived reads no older than the
    /// node's uptime ([`WireSnapshot::uptime_ms`]). Absent = a terminal order, or a node that predates
    /// the field: render unknown, never zero (a missing age read as 0 would call every order new).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
}

/// One net/hedge position leg — mirrors the display subset of `vike_exec::PositionView`.
/// `position_side` is the leg label (`"BOTH"`/`"long"`/`"short"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WirePositionView {
    pub venue: String,
    pub symbol: String,
    pub position_side: String,
    pub size: f64,
    pub avg_px: f64,
    pub unrealized: f64,
    /// Effective leverage (`1/im`); 0.0 when the margin gate is off.
    pub leverage: f64,
    /// Estimated liquidation mark; 0.0 when not liquidatable / gate off.
    pub liq_price: f64,
}

/// One PENDING bracket exit held off the venue until its OTO parent fills — mirrors
/// `vike_exec::HeldOrderView`. NOT in [`WireSnapshot::orders`] (no venue order yet);
/// `parent_order_id` names the entry whose fill releases the leg.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireHeldOrderView {
    pub client_order_id: String,
    pub venue: String,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    pub parent_order_id: Option<String>,
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

/// What one unit of one symbol's quantity is worth — the NON-DEFAULT rows of an engine's
/// `vike_exec::VenueBlock::contract_values` grid (decision 0170), so a remote client's order preview
/// sizes by the same number the node's gate does. Carried as text and a number: this crate links no
/// `vike-model` (decision 0091 §3), so `kind` is the `NotionalKind` word (`"linear"`/`"inverse"`),
/// parsed at the client's edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireContractValue {
    /// The symbol the row is for.
    pub symbol: String,
    /// `"linear"` or `"inverse"` (`vike_model::NotionalKind`'s serde word).
    pub kind: String,
    /// Base units (linear) or quote-currency units (inverse) one unit of quantity stands for.
    pub per_unit: f64,
}

/// A per-venue ledger block — mirrors the display fields of `vike_exec::VenueBlock`; the heavy
/// internals (fee schedule, margin math) stay core-side, and the contract-value grid rides as its
/// non-default rows ([`WireContractValue`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireVenueBlock {
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
    pub free_bp: f64,
    pub trading_state: WireTradingState,
    pub positions: Vec<WirePositionView>,
    /// WHICH ACCOUNT of `venue` this block is; absent for the unlabelled one (the
    /// `vike_model::accounts::account_keys::AccountLabel` convention). Mirrors
    /// `vike_exec::VenueBlock::account`.
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
    /// [`WireDirectoryAccount::tier`] because this crate links no `vike-model` (decision 0091 §3).
    /// An account runs at its tier or not at all, so it is also what an order sent to it goes to.
    ///
    /// ⚠ `None` = the node did not say: render unknown and treat as `live` wherever safety is at
    /// stake, never guess. A word the reader's parser refuses reads the same. ⚠ Its PRESENCE is
    /// the tell that this node names each ORDER's account ([`WireOrderView::account`]).
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
    pub contract_values: Vec<WireContractValue>,
    /// **This engine takes an order on a symbol it was not started on** (decisions 0173/0174: it
    /// admits the symbol on the first order). Mirrors `vike_exec::VenueBlock::admits_symbols`, which
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
    /// `vike_exec::VenueBlock::quote_rates`. [`Self::contract_values`] already carries the rate multiplied in
    /// (the EFFECTIVE value); this table says WHICH rate that was and lets a client show that it is stale
    /// (an opening order on such a symbol is denied while it is). EMPTY for every account without a symbol
    /// quoted in another currency than its own, which is every account on a venue quoted in its own, and
    /// exactly what a node that predates the field implies; absent from the frame when empty, so such a block
    /// is byte-identical to the one an older node writes. An older client ignores the key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quote_rates: Vec<WireQuoteRate>,
    /// The currency [`Self::quote_rates`] is expressed in (the engine's ledger currency: the account's own).
    /// EMPTY exactly when the table is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote_rates_in: String,
    /// **The currency GROUP this block's money is in** (decision 0183, item 13): mirrors
    /// `vike_exec::VenueBlock::ledger_ccy`. EMPTY for the dollar group (`USD`, `USDT`, an account whose
    /// currency nobody told: every account of today's nodes), else the upper-cased code (`"EUR"`). A
    /// total across blocks may add only the blocks of one group, so [`WireSnapshot::equity_total`] is the
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
    pub ledger_ccy: String,
}

/// One row of [`WireVenueBlock::quote_rates`]: what one unit of `ccy` is worth in the account's currency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireQuoteRate {
    /// The quote currency of one or more symbols of the account (`"JPY"`).
    pub ccy: String,
    /// Account-currency units per ONE unit of `ccy` (USDJPY on a USD account: about `1/150`).
    pub rate: f64,
    /// When the node observed it, epoch ms on the node's clock.
    pub as_of_ms: i64,
    /// Older than the engine's staleness window at the instant of this frame, or never priced.
    pub stale: bool,
}

/// `skip_serializing_if` for a `bool` that is absent when `false`.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// **One account the node knows and does NOT run** — mirrors `vike_exec::NotRunningAccount`
/// (decision 0156: an account runs at the tier its active row states or not at all, never as a
/// paper engine in its place). It has no [`WireVenueBlock`] because no engine stands behind it; this
/// is what lets a client show WHY instead of a bare "not running".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireNotRunning {
    /// The venue id.
    pub venue: String,
    /// WHICH ACCOUNT of `venue`; absent for the unlabelled one ([`WireVenueBlock::account`]'s
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
    /// WHY it does not run, as the node's arming block words it (`vike_config::ArmingBlock::as_str`:
    /// `no credentials`, `flags.tradehub_live off`, …). A word, shown as given.
    pub reason: String,
}

/// One OHLCV candle — mirrors the display subset of `vike_marketdata::Bar`. Short field names keep the
/// body small (a series carries up to ~300 of these).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireBar {
    /// Open-time epoch ms.
    pub ts: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    /// Volume.
    pub v: f64,
}

/// One `(venue, symbol, interval)` bar tail — mirrors `vike_exec::BarSeries`. `closed` is the
/// LAST-K closed bars (capped NODE-side: the core's own vec is unbounded); `forming` is the live
/// in-progress candle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireBarSeries {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub closed: Vec<WireBar>,
    pub forming: Option<WireBar>,
}

/// The rendered core state view the node publishes — mirrors the display projection of
/// `vike_exec::CoreSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSnapshot {
    /// Per-publish sequence number (GUI change detection).
    pub seq: u64,
    /// Primary engine's venue.
    pub venue: String,
    /// Primary engine's symbol.
    pub symbol: String,
    /// Primary engine trading state.
    pub trading_state: WireTradingState,
    /// Primary venue cash balance.
    pub balance: f64,
    /// Cross-venue total equity (the core's `py_sum` `equity_total`): the blocks of the PRIMARY block's
    /// currency group only ([`WireVenueBlock::ledger_ccy`]); on a node whose engines share one currency,
    /// every block.
    pub equity_total: f64,
    /// Per-venue ledger blocks (primary first, then extras in registration order).
    pub venues: Vec<WireVenueBlock>,
    /// Order registry (insertion order), spanning every engine.
    pub orders: Vec<WireOrderView>,
    /// Top-level (primary-venue) positions — the mirror of `venues[0].positions`.
    pub positions: Vec<WirePositionView>,
    /// Pending bracket exits held until their OTO parent fills; empty on non-bracket runs.
    pub held_exits: Vec<WireHeldOrderView>,
    /// Recent delivered exec events (the bounded journal tail).
    pub recent_events: Vec<String>,
    /// Set once a handler panicked — the core is HALTED in safe-state.
    pub fault: Option<String>,
    /// Bounded bar tails per mounted series (LAST-K closed + forming), projected node-side. Empty
    /// when the node has no bars yet or predates the field.
    #[serde(default)]
    pub bars: Vec<WireBarSeries>,
    /// Which daemon this is ([`WireHubIdentity`]); `None` from a node that predates the field.
    #[serde(default)]
    pub identity: Option<WireHubIdentity>,
    /// A digest of the node's MOUNTED ACCOUNT SET (`vike_exec::CoreSnapshot::accounts_epoch`),
    /// changing exactly when the set of route keys does. `0` = predates the field, or no accounts.
    ///
    /// ⚠ It answers ONE question: did the set move between a preview and its confirm? A client
    /// stamps it into the preview and refuses a stale confirm (*"the node's account set changed
    /// since your preview, re-preview"*). A DIGEST, not a counter, which would restart at zero with
    /// the daemon (`vike_core::snapshot::accounts_epoch_of`).
    #[serde(default)]
    pub accounts_epoch: u64,
    /// **Every account the node knows and does NOT run, each with why** ([`WireNotRunning`],
    /// decision 0156). Empty = every account the node knows runs, or a node that predates the
    /// field: a client then says only that an account it cannot find among [`Self::venues`] is not
    /// running, as it always did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_running: Vec<WireNotRunning>,
    /// **The ROUTE KEYS of the RUNNING accounts whose key in the settings database is no longer the key
    /// their session started with** ([`KEY_CHANGED_WARNING`] is the sentence to show for each). The session
    /// keeps signing with the old key until that account restarts; nothing restarts it by itself, because a
    /// restart cancels its resting orders. Words only: no fingerprint and no value is on the wire. Empty =
    /// none changed, or a node that predates the field (byte-identical frames).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_changed: Vec<String>,
    /// **The RUNNING accounts whose venue balance the node HOLDS BACK from their engine** ([`WireBalanceHeld`]):
    /// their equity is the engine's own book (seed + PnL), not the venue's wallet, until the hold ends. Words
    /// only. Empty = none held, or a node that predates the field.
    ///
    /// ⚠ **No feature string, decision 0112 verdict 3**: a pure addition to a reply, written only when
    /// non-empty, so a frame with nothing held is byte-identical to the one an older node writes and an older
    /// observer ignores the key (an older observer simply shows nothing, as it always did).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub balance_held: Vec<WireBalanceHeld>,
    /// **Every reconcile divergence the core is HOLDING for an operator** ([`WireReconAlert`]),
    /// the rows `trade recon ls` lists and `WireCommand::ReconConfirm` resolves by id. Empty =
    /// nothing is held, or a node that predates the field (check
    /// [`crate::proto::FEATURE_RECON_VERBS`], never the emptiness).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recon: Vec<WireReconAlert>,
    /// **How long the node PROCESS has been running, in milliseconds, at the moment this frame was
    /// built** (the publisher's clock, from the process start - `crates/vike-tradehub/src/publish.rs`'s
    /// `process_started`). A reading of the frame, not of the identity block: it changes with every
    /// frame, and [`WireHubIdentity`] is the process-static block a client compares for equality.
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
    /// ledger beside USDT, see [`WireVenueBlock::ledger_ccy`]; its blocks carry no key either, so the
    /// observer sees one group and adds them, as v0.1.81's did).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub primary_ccy: String,
}

/// One held reconcile divergence: the wire twin of `vike_exec::ReconAlertView`. Never the
/// proposed events themselves - those stay fold-thread state until an operator confirms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireReconAlert {
    /// The key `WireCommand::ReconConfirm` resolves by: a 48-bit digest of the divergence's
    /// identity (venue, account, kind, instrument), so the SAME divergence has the SAME id after a
    /// restart and a retried confirm can only reach it (or nothing). It was a per-run counter, and
    /// `confirm 3` after a restart landed on whatever divergence the new run held as #3.
    pub id: u64,
    /// WHICH BOOK the divergence is about: the account's route key (the bare venue id for a
    /// venue's default account).
    pub account: String,
    /// The `DivergenceKind` name (`OrphanLocalPosition`, `BalanceDrift`, ...).
    pub kind: String,
    /// The operator-readable description.
    pub detail: String,
    /// How many events a confirm folds (0 for an acknowledgement-only or drop-only kind).
    pub proposed_event_count: usize,
}

/// The sentence an operator surface shows for a RUNNING account listed in [`WireSnapshot::key_changed`].
pub const KEY_CHANGED_WARNING: &str =
    "key changed in the database - the old key is still in use until a restart";

/// One account whose venue balance the node holds back from its engine ([`WireSnapshot::balance_held`]): the
/// node-side balance gate (`vike-tradehub`'s `remote_backend::balance_gate`) found the cash the venue reports
/// cannot be shown to be in the currency the engine's ledger is labelled with, so the engine's equity stays
/// its own book (seed + PnL) instead of the venue's wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireBalanceHeld {
    /// The account's ROUTING key (the bare venue id for the default account, else `venue#LABEL`).
    pub route_key: String,
    /// WHY, in short words, shown as given: `assumed USD` (the venue stated no currency and the node
    /// assumed USD), `stated EUR not anchored` (the venue stated a currency the engine's ledger could not be
    /// anchored to), `untold foreign stock` (a stock in another currency the daemon was not told yet).
    pub reason: String,
}

/// What a held balance means for the account, said once for every operator surface.
pub const BALANCE_HELD_MEANING: &str = "the venue's cash is not adopted, so the engine's equity is \
     its own book (seed + PnL) until the hold ends";

impl WireBalanceHeld {
    /// `balance held: <reason> - <meaning>`, for a surface that already names the account (a doctor row's scope).
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

impl WireSnapshot {
    /// An empty placeholder (`seq: 0`) — the wire twin of `vike_exec::CoreSnapshot::empty`.
    /// [`crate::remote_handle::RemoteCoreHandle::snapshot`] returns it before the first frame, so a
    /// reader always has a value to render (the "connecting" state).
    pub fn empty() -> Self {
        WireSnapshot {
            seq: 0,
            venue: String::new(),
            symbol: String::new(),
            trading_state: WireTradingState::Active,
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
            balance_held: Vec::new(),
            recon: Vec::new(),
            uptime_ms: None,
            halt_sentinel: None,
            primary_ccy: String::new(),
        }
    }
}

/// WHICH daemon a [`WireSnapshot`] describes — name, mounted strategy, paper-vs-live, build — so a
/// GUI holding several backends can label them. Additive (the module's additive-field contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireHubIdentity {
    /// Operator-facing daemon name (the profile file stem today).
    pub name: String,
    /// The mounted strategy's registry name.
    pub strategy: String,
    /// The strategy's effective params, rendered (`DaemonProfile::effective_params`).
    pub params: String,
    /// True iff the daemon is LIVE (`flags.tradehub_live`); false = paper.
    pub live: bool,
    /// The daemon's build identity (`vike_buildinfo::summary`).
    pub build: String,
    /// WHICH BOX the daemon runs on, as the daemon reports it: the client cannot derive it (behind
    /// the SSH tunnel every client dials `127.0.0.1:7879`). ⚠ A CLAIM, not a verified fact, naming
    /// a box rather than a dialable endpoint. Empty = nothing was said (older node, or a
    /// loopback-only box with `config.tradehub_advertise_addr` unset), never 127.0.0.1. Produced in
    /// `crates/vike-tradehub/src/self_address.rs`, rendered by
    /// `vike_app_core::backend::backend_identity`.
    #[serde(default)]
    pub advertise_addr: String,
}

/// One mounted strategy in a [`WireStrategyStatus`]: one row per mount, in mount order.
///
/// The addressing key a [`WireCommand::UpdateParams`] targets (`venue`/`symbol`/`interval`/
/// `mount_id`) is on the row, so no client string-parses `vike-tradehub`'s `mounts_wire_params`
/// prefix.
///
/// ⚠ Every field below `live` is `#[serde(default)]` and reads EMPTY from a node that predates
/// [`crate::proto::FEATURE_STRATEGY_PARAMS`], indistinguishable from "this mount publishes no
/// typed params": **check the capability, never the emptiness.**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireMountRow {
    /// The mounted strategy's registry name (e.g. `"spread_maker"`).
    pub strategy: String,
    /// The mount's effective params, rendered (`DaemonProfile::effective_params`). Boot-time
    /// prose, stale after the first `UpdateParams`: a read-modify-write reads
    /// [`Self::typed_params`].
    pub params: String,
    /// True iff this mount trades LIVE; false = paper. ⚠ A per-VENUE fact from
    /// `vike_tradehub_core::assemble_runtime`'s `live_venues` (a real `ExecutionClient` was built), not the
    /// daemon's gate: a mount without credentials, `data_only`, or demoted to paper reads `false`
    /// while [`WireHubIdentity::live`] reads `true` — the normal, informative disagreement.
    pub live: bool,
    /// This mount's VENUE (part of the [`WireCommand::UpdateParams`] key).
    #[serde(default)]
    pub venue: String,
    /// This mount's SYMBOL — see [`Self::venue`].
    #[serde(default)]
    pub symbol: String,
    /// This mount's INTERVAL — see [`Self::venue`]. Two mounts may differ only by it.
    #[serde(default)]
    pub interval: String,
    /// This mount's ID (`WireCommand::UpdateParams::mount_id`), the part of the key that separates
    /// two mounts on one series: the sanitized `controller_id` or the derived
    /// `{venue}__{symbol}__{interval}`. `""` from a node that predates
    /// [`crate::proto::FEATURE_PARAMS_BY_MOUNT`] and on a row the live overlay could not match.
    #[serde(default)]
    pub mount_id: String,
    /// The mount's LIVE tunables (read off the live core) as `vike_model::StrategyParams` serde
    /// JSON — the exact shape [`WireCommand::UpdateParams`] takes back. `None` = EITHER no typed
    /// params (`UpdateParams` cannot address it either) OR a node predating the capability;
    /// `Welcome.features` tells them apart.
    #[serde(default)]
    pub typed_params: Option<serde_json::Value>,
    /// WHAT PRODUCT this mount trades, as the `vike_model::AssetClass::sql_word` (`"CryptoPerp"`,
    /// …; equal to its serde spelling, parse with `AssetClass::from_sql_word`). ⚠ `None` from a
    /// node predating [`crate::proto::FEATURE_MOUNT_CLASS`] looks like a current TOML-backed mount
    /// (`docs/decisions/0061-an-instrument-names-its-kind.md` Phase 5): check the capability.
    /// ⚠ DISPLAY ONLY: per 0061 not a routing input.
    #[serde(default)]
    pub asset_class: Option<String>,
}

/// The payload of a `Response::StrategyStatus`: the node's identity block plus its mounts.
/// `effective_params` is the daemon's one resolved-params line (the identity block's string), so a
/// client that only wants "what is this node running" skips the mounts.
///
/// ⚠ No `Eq` here or on [`WireMountRow`]: the row holds a [`serde_json::Value`] (same reason
/// [`WireCommand`] derives only `PartialEq`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireStrategyStatus {
    /// WHICH daemon answered — the same identity block every [`WireSnapshot`] carries.
    pub identity: WireHubIdentity,
    /// The daemon's resolved effective-params line (`DaemonProfile::effective_params`).
    pub effective_params: String,
    /// One row per mounted strategy.
    pub mounts: Vec<WireMountRow>,
}

/// One effective settings row — the wire rendering of `vike_config::show`'s `FileRow`, the rows
/// `vike-cli config show` prints. STRINGLY on purpose (no `vike-config` dependency); secret-shaped
/// values arrive already redacted by the shared builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSettingsRow {
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
pub struct WireSettingsShow {
    /// The settings directory the node resolved at boot, rendered. `None` = compiled-in defaults
    /// (no project above its working directory).
    pub settings_dir: Option<String>,
    /// One row per typed settings key, sorted by key.
    pub rows: Vec<WireSettingsRow>,
}

/// One order intent — mirrors the serde fields of `vike_model::OrderRequest`. An empty
/// `client_order_id` asks the runtime to mint one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireOrderRequest {
    #[serde(default)]
    pub client_order_id: String,
    pub venue: String,
    pub symbol: String,
    /// +1 buy / -1 sell.
    pub side: i32,
    pub qty: f64,
    /// `"market"` | `"limit"` | `"stop"` | `"stop_limit"` | `"take_profit"`, carried verbatim; the
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
}

/// One TP/SL BRACKET — mirrors `vike_model::BracketSpec` field for field: an entry (market when
/// `entry_price` is `None`, else limit) plus a stop-loss and a take-profit, which the core holds
/// until the entry fills and then sends as reduce-only OCO siblings (`vike_model::build_bracket`).
///
/// ⚠ **No client order id, because `BracketSpec` has none.** A bracket that names NO account is
/// accepted only on the default account of a venue with exactly ONE engine; one that names an
/// account ([`Self::account`]) goes to that account's engine. Either way the engine must trade the
/// bracket's symbol, and on binance and aster must be a PERP; the node also refuses a side other
/// than ±1, non-finite or non-positive sizes/prices and exits on the wrong side, all before its
/// `Ack` (`vike-tradehub`'s `account_refusal` and `bracket_refusal`). The core mints the three ids
/// after the `Ack`; the entry id reaches the client as each held exit's `parent_order_id`.
///
/// ⚠ **`deny_unknown_fields` is load-bearing**: an older node must ERROR on a key it does not know
/// rather than drop it and route to the default book (that is how [`Self::account`] stays safe
/// against a node from before it). Never relax it to add a field; a new field rides a new
/// `Welcome.features` word ([`crate::proto::FEATURE_BRACKET_ACCOUNT`] for this one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireBracketSpec {
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
    /// `crate::remote_control`). `crate::wire::wire_tests`'
    /// `a_non_finite_entry_price_round_trips_to_a_market_entry` pins it.
    #[serde(deserialize_with = "Option::<f64>::deserialize")]
    pub entry_price: Option<f64>,
    /// The stop-loss trigger price.
    pub stop_loss: f64,
    /// The take-profit limit price.
    pub take_profit: f64,
    /// WHICH ACCOUNT of `venue`; `None` = the sender named none ([`WireOrderRequest::account`]'s three
    /// wire states: absent = named nothing, `"DEFAULT"` = the unlabelled account, a label = that one).
    /// Requires [`crate::proto::FEATURE_BRACKET_ACCOUNT`] (refused client-side otherwise).
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing**: a bracket naming no account is BYTE-IDENTICAL to
    /// the pre-field frame, so it still decodes on a node from before the field (whose
    /// `deny_unknown_fields` would refuse the key). And `default` is what lets this node read that old
    /// frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// The command vocabulary a [`Scope::Write`](vike_hub_proto::auth::Scope) client may issue — a mirror of
/// the SESSION-relevant verbs in `vike_exec::{Command, OrderIntent}` (orders, brackets, cancels,
/// flatten, trading state, strategy params/mounts, settings writes, and the operator's
/// `ReconConfirm` of one held reconcile divergence). NOT the rest of the reconcile/journal
/// surface (`ApplySnapshot`, `ReconcileReports`, …): that is core-internal plumbing and never a
/// remote client's business. A server lowers each variant into the core's
/// `Command`/`OrderIntent` at its edge.
///
/// ⚠ `Debug` stays DERIVED: no variant may carry a credential value (0065 §4.1). The account verbs
/// that do are [`AccountRequest`], outside this enum, with a redacting `Debug`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireCommand {
    /// Submit one order (`OrderIntent::Submit`).
    Submit(WireOrderRequest),
    /// Submit one TP/SL BRACKET (`OrderIntent::Bracket`). Naming no account reaches only the default
    /// book of a one-engine venue — see [`WireBracketSpec`]. Requires
    /// [`crate::proto::FEATURE_BRACKET`], and [`crate::proto::FEATURE_BRACKET_ACCOUNT`] when it names an
    /// account (refused client-side otherwise).
    Bracket(WireBracketSpec),
    /// Cancel one order by client-order-id (`OrderIntent::Cancel`).
    Cancel(String),
    /// Modify one resting order's qty and/or price (`OrderIntent::Modify`).
    Modify { client_order_id: String, new_qty: Option<f64>, new_price: Option<f64> },
    /// Cancel every live order, optionally scoped to a venue/symbol (`OrderIntent::MassCancel`).
    /// `None`/`None` = all engines + all books.
    ///
    /// An absent `account` = every account of the venue, deliberately. A named one cancels only
    /// that book, on a node advertising `crate::proto::FEATURE_ACCOUNT_SCOPED_REDUCE` (it refuses
    /// an unknown account, or one with no `venue`, before the Ack); against an older node, which
    /// would cancel every account, the frame is refused client-side.
    MassCancel {
        venue: Option<String>,
        symbol: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// Close the `(venue, symbol)` net position with a reduce-only market order
    /// (`OrderIntent::Flatten`); no-op when flat. An absent `account` closes it on every account of
    /// `venue`; a named one narrows it, under [`WireCommand::MassCancel`]'s capability/refusals.
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
    /// `MassCancel`'s capability and refusals (`crate::remote_control::required_feature`).
    MarketExit {
        venue: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// Set the account trading state / kill switch (`Command::SetTradingState`).
    SetTradingState(WireTradingState),
    /// LIVE PARAMETER update for a mounted strategy (`Command::UpdateParams` /
    /// `vike_exec::ParamsUpdate`). `(venue, symbol, interval)` names the series EXACTLY and
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
        /// WHICH mount on the series: a `WireMountRow::mount_id` (matched as the core stores it,
        /// so `maker-a` finds `maker_a`). `None`: delivered when exactly one mount sits on the
        /// series, refused when several share it.
        ///
        /// ⚠ Gated by [`crate::proto::FEATURE_PARAMS_BY_MOUNT`], NOT `strategy-verbs`: an older
        /// node drops the key and retunes the FIRST mount behind a normal Ack, so the client
        /// refuses it there ([`crate::remote_control`]'s `required_feature`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mount_id: Option<String>,
        /// The typed `vike_model::StrategyParams` payload, in the core's own serde JSON form.
        params: serde_json::Value,
    },
    /// RUNTIME strategy MOUNT (`Command::MountStrategy` / `vike_exec::MountSpec`) without a
    /// restart. The source is the profile `[strategy]` vocabulary: a registry `name` XOR a `rhai`
    /// script path (docs/decisions/0024-rhai-strategies-live.md), plus `params` as JSON. The daemon
    /// validates at its edge like a profile load (`Response::Error`); a refusal the core raises
    /// later (duplicate id, unknown venue) lands in recent-events. Requires
    /// [`crate::proto::FEATURE_MOUNT_VERBS`].
    MountStrategy {
        /// Target venue — must name an engine the node's core already runs.
        venue: String,
        /// WHICH ACCOUNT of `venue` the mount trades and reads ([`WireOrderRequest::account`]'s
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
        /// Registry strategy name — exactly one of `name`/`rhai`.
        name: Option<String>,
        /// Rhai script PATH on the NODE's filesystem — exactly one of `name`/`rhai`.
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
    /// STRINGLY, like `WireSettingsRow`: the daemon is the typing authority.
    ///
    /// `Response::SettingsWritten`'s `restart_required` is the node's per-key call: `false` only
    /// for a hot-safe key it applied live (never `policy.*`). Requires
    /// [`crate::proto::FEATURE_SETTINGS_WRITE`] (`crate::remote_control::set_setting`).
    ///
    /// ⚠ **`file` and `confirm` are FILE-ERA fields the node ignores** (0086 point 7) and decode
    /// as absent, yet every client KEEPS SENDING `file` (the key's first segment) and
    /// `confirm: None` until they are deleted: the released v0.1.35 daemon REQUIRES `file`.
    SetSetting {
        /// IGNORED by the node — see the variant doc. A client sends the key's section word.
        #[serde(default)]
        file: String,
        /// The FULL dotted key as `WireSettingsRow::key` renders it
        /// (`"policy.max_notional_per_order"`): first segment = section.
        key: String,
        /// The new value as TEXT: parsed as a TOML value when it is one, else a string; then the
        /// loader validates it.
        value: String,
        /// IGNORED by the node (0086 point 7); every client sends `None`.
        #[serde(default)]
        confirm: Option<String>,
    },
    /// CONFIRM one HELD reconcile divergence by its id (`Command::ConfirmRecon`): the core folds
    /// what the divergence proposed into the account's book - the venue's position for
    /// `PositionOnlyExternal`/`PositionDrift`, the missed fill for `MissingFill`, the venue's cash
    /// figure for `BalanceDrift`, and for `OrphanLocalPosition` the DROP of the local position the
    /// venue never reported (one closing fill at the position's own average price: zero PnL).
    /// `id` is a [`WireReconAlert::id`]; an unknown, already-confirmed or since-cleared id is a
    /// note in recent-events, never an error (and a refusal: nothing folds). The id is the
    /// divergence's identity digest, so it names the same divergence after a node restart.
    /// Names no venue and no account: the held row carries the book it belongs to. Requires
    /// [`crate::proto::FEATURE_RECON_VERBS`].
    ReconConfirm {
        /// The held divergence's id (`WireReconAlert::id`).
        id: u64,
    },
    /// CANCEL, at the venue, the ONE order a held `UnknownOrder` row names (`Command::CancelReconOrder`,
    /// decision 0196 rule (b)): the core sends a single cancel for the client id the row carries,
    /// through that account's own client. `demo`-tier accounts only (any other tier, a row of
    /// another kind, a row with no id the venue can cancel by, or an id the node no longer holds is a
    /// refusal note in recent-events and a WARN, nothing sent, still an `Ack`: the verdict is the
    /// core's). It folds nothing and removes no row; the row clears at the next reconcile pass that no
    /// longer raises it. Operator only: the CLI's `trade recon cancel <id>` is its one constructor,
    /// and no MCP tool, GUI call or strategy builds it. Names no venue and no account: the held row
    /// carries the book it belongs to. Requires [`crate::proto::FEATURE_RECON_CANCEL`], refused
    /// client-side otherwise: a node that predates it cannot decode the variant.
    ReconCancel {
        /// The held row's id (`WireReconAlert::id`).
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
    /// node's edge before the `Ack` ([`WireCommand::addressed_venue`]). Requires
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
    /// An engine the link dead-man holds `Halted` stays so (`resume-venue` is its way out).
    ///
    /// `venue` and `account` name the book as every account-scoped verb does (`account` is
    /// `parse_wire_account`'s: `DEFAULT` or a label). An account this node does not run is refused at the
    /// node's edge before the `Ack` (`account_refusal`), naming the accounts it does run. Requires
    /// [`crate::proto::FEATURE_ACCOUNT_TRADING_STATE`], refused client-side otherwise: a node that predates
    /// it cannot decode the variant.
    SetAccountTradingState { venue: String, account: String, state: WireTradingState },
}

impl WireCommand {
    /// **The VENUE this command ADDRESSES**, or `None` when it addresses none — the ONE addressing
    /// key on this wire (the daemon's `lower_command` copies it onto
    /// `vike_model::OrderRequest::venue`; `vike_core`'s `CoreThread::route_of` resolves it).
    ///
    /// ⚠ `Some(v)` = "act on venue `v`", which a node with no engine for `v` must REFUSE
    /// (`vike-tradehub`'s `venue_refusal`; [`crate::proto::FEATURE_VENUE_ROUTING`]). `None` is no
    /// omission: an ORDER-scoped verb (`Cancel`/`Modify`, resolved by the core's `coid_venue`
    /// map), an ACCOUNT-wide verb (`SetTradingState`, the `venue: None` arms of
    /// `MassCancel`/`MarketExit`), or a verb about no book (`UnmountStrategy`, `SetSetting`).
    /// ⚠ NO WILDCARD ARM: a new variant must be classified by hand, or a new order verb would
    /// silently become address-less.
    pub fn addressed_venue(&self) -> Option<&str> {
        match self {
            WireCommand::Submit(req) => Some(req.venue.as_str()),
            WireCommand::Bracket(b) => Some(b.venue.as_str()),
            WireCommand::Flatten { venue, .. } => Some(venue.as_str()),
            WireCommand::UpdateParams { venue, .. } => Some(venue.as_str()),
            WireCommand::MountStrategy { venue, .. } => Some(venue.as_str()),
            // The exchange whose link-dead-man halt is released: a node with no engine for it
            // refuses before the `Ack` instead of Acking a no-op.
            WireCommand::ResumeVenue { venue, .. } => Some(venue.as_str()),
            // The exchange of the account whose state is set: a node with no engine for it refuses
            // before the `Ack`, in the venue gate's own words.
            WireCommand::SetAccountTradingState { venue, .. } => Some(venue.as_str()),
            // `Some(v)` = every account of that exchange (`CoreThread::exit_scope_engines`);
            // `None` = every engine, never refused.
            WireCommand::MassCancel { venue, .. } => venue.as_deref(),
            WireCommand::MarketExit { venue, .. } => venue.as_deref(),
            WireCommand::Cancel(_)
            | WireCommand::Modify { .. }
            | WireCommand::SetTradingState(_)
            | WireCommand::UnmountStrategy { .. }
            | WireCommand::SetSetting { .. }
            | WireCommand::ReconConfirm { .. }
            | WireCommand::ReconCancel { .. } => None,
        }
    }
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;

#[path = "the_account_field_is_invisible_until_it_is_used.rs"]
#[cfg(test)]
mod the_account_field_is_invisible_until_it_is_used;
