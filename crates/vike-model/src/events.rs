//! Immutable order/fill/position events. Exact port of `exec/events.py`.
//!
//! Every type is a plain value struct (Clone, no interior refs) so events are copy-safe across
//! the channel boundary — the Rust twin of Python's frozen-dataclass rule. The `Event` enum is
//! the tagged union the core loop consumes; fixtures serialize it with a `"type"` tag.
//!
//! ## Interning contract (why `venue`/`symbol` are [`Ustr`], and what MUST NOT be)
//!
//! `venue`/`symbol` are interned ([`Ustr`]): 8-byte `Copy`, pointer-equality, serde-transparent.
//! The `ustr` global table **never frees** an interned string, so interning is only sound for
//! fields whose set of *distinct values over a process lifetime* is BOUNDED. That holds here:
//! - venue: ~10 values ever.
//! - symbol: bounded by *distinct instruments subscribed per session*. Crypto is a handful.
//!   Polymarket's `symbol` is the 77-char `token_id`, but it is minted **per market, reused on
//!   every tick/fill of that market** — so the interned set = distinct markets subscribed
//!   (hundreds–low-thousands per session ⇒ single-digit MB), NOT trade count.
//!
//! This is exactly why `trade_id` is NOT interned: it is minted **per fill**, unbounded, so a
//! `Ustr` there would leak without bound (Nautilus shipped and then reverted precisely this bug —
//! their `TradeId` moved off `Ustr` to a stack string). High-cardinality / per-event id fields
//! use `CompactString`; only bounded, repeated label fields use `Ustr`. Do not intern secrets
//! (they can't be zeroized) or free text. See [`prewarm_interner`] for the one-time init cost.
//!
//! [`TradeId`] preserves that property EXACTLY: it is a newtype whose single field is the same
//! [`CompactString`] the bare field used to be, so the storage, the allocation behaviour and the
//! "not interned" guarantee are unchanged — the newtype adds a constructor gate and takes away
//! `Default`, and adds no interning and no global table.

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use ustr::Ustr;

/// A venue execution id that **cannot be empty**.
///
/// ## Why this is a type and not a validated `CompactString`
///
/// `trade_id` is the dedup key for the whole money lane. `ExecutionEngine::on_event` guards
/// `Account::apply_fill` with it, and `vike_exec::recon::diff` decides whether a venue-reported
/// fill is already known by looking it up in `seen_trade_ids`. Both guards used to read
/// `if !fill.trade_id.is_empty()` — so an EMPTY id did not merely dedup badly, it **skipped dedup
/// entirely** and the fill was applied unconditionally. Measured consequence on a live daemon:
/// re-applied fills double-booked commission and realized PnL. On the recon side an empty id is
/// worse still: it can never match `seen_trade_ids`, so it manufactures a `MissingFill`
/// divergence, and `MissingFill` is one of the two kinds `hybrid` policy AUTO-APPLIES.
///
/// Empty ids were produced, not hypothetical: five venue mappers reached the field through
/// `unwrap_or_default()`, which on a string is `""`.
///
/// The cure is the ONE omission that turns every such site into a compile error: this type
/// deliberately implements **no [`Default`]**. `unwrap_or_default()` therefore cannot name it, and
/// the sites are found by the compiler instead of by grep — including sites a future adapter author
/// has not written yet. Nautilus reaches the same property the same way (`TradeId::new` rejects
/// `""`, and `trade_id` is a non-`Option` `TradeId` on both its fill event and its fill report).
///
/// ## Constructing one
///
/// | you have | use | on empty |
/// |---|---|---|
/// | venue wire data | [`TradeId::new`] | `Err(`[`EmptyTradeId`]`)` — caller decides |
/// | a source literal | `"t1".into()` | panics (a source bug, not venue data) |
/// | a synthesized id | [`TradeId::prefixed`] | unreachable — non-empty by construction |
///
/// There is deliberately **no** `From<String>`, no `From<&str>` for a non-`'static` lifetime and no
/// panicking `new`. That split is load-bearing rather than stylistic: venue JSON borrows from the
/// parsed document and is never `&'static`, so `From<&'static str>` accepts test/source literals
/// while every wire value is forced through the fallible [`TradeId::new`]. A venue mapper on the
/// ingest path must therefore handle the malformed-frame case explicitly, and CANNOT reach a
/// panicking constructor — a malformed frame must never kill a live daemon (the same rule the
/// hostile-venue finiteness guard follows: refuse the event, keep the process).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TradeId(CompactString);

/// A `trade_id` was empty. Carried as an error rather than panicking because the producer is a
/// venue frame on the live ingest path — see [`TradeId`]'s constructor table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyTradeId;

impl std::fmt::Display for EmptyTradeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("trade_id is empty (it is the fill dedup key and cannot be)")
    }
}

impl std::error::Error for EmptyTradeId {}

impl TradeId {
    /// The wire constructor: refuses an empty id. Use this for anything read off a venue frame.
    ///
    /// The caller decides what an empty id means for THAT venue — drop the event, synthesize a
    /// deterministic id, or propagate. There is no default answer because there is no answer that
    /// is right for every venue.
    pub fn new(value: impl AsRef<str>) -> Result<Self, EmptyTradeId> {
        let s = value.as_ref();
        if s.is_empty() {
            return Err(EmptyTradeId);
        }
        Ok(Self(CompactString::new(s)))
    }

    /// A synthesized id built from a NON-EMPTY static prefix — infallible **by construction**,
    /// because a non-empty prefix cannot yield an empty result however `rest` renders.
    ///
    /// This is the sanctioned shape for every id vike mints itself (`EXT-ORD-…`, `paper-…`,
    /// `NETRA-…`), and it exists so those sites need neither an `unwrap` nor a `Result` they would
    /// only ever discard. ⚠ A synthesized id must be a function of fields that are STABLE across a
    /// replay — never a wall-clock timestamp or an in-process counter, both of which differ on the
    /// second run and so defeat the dedup the id exists to feed.
    ///
    /// # Panics
    /// If `prefix` is empty. `prefix` is `&'static str`, so that is a source-level bug reachable by
    /// any test touching the call site, never a venue-data condition.
    pub fn prefixed(prefix: &'static str, rest: impl std::fmt::Display) -> Self {
        assert!(!prefix.is_empty(), "TradeId::prefixed needs a non-empty static prefix");
        Self(CompactString::new(format!("{prefix}{rest}")))
    }

    /// The id as a string slice. Never empty.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// True when this id carries one of the synthetic namespaces vike mints. Useful for reporting;
    /// never for correctness.
    pub fn starts_with(&self, prefix: &str) -> bool {
        self.0.starts_with(prefix)
    }
}

impl std::fmt::Display for TradeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl std::ops::Deref for TradeId {
    type Target = str;
    fn deref(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for TradeId {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl std::borrow::Borrow<str> for TradeId {
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq<str> for TradeId {
    fn eq(&self, other: &str) -> bool {
        self.0.as_str() == other
    }
}

impl PartialEq<&str> for TradeId {
    fn eq(&self, other: &&str) -> bool {
        self.0.as_str() == *other
    }
}

/// Source literals only — see [`TradeId`]'s constructor table for why this is `&'static str` and
/// not `&str`.
///
/// # Panics
/// If the literal is empty. Unreachable from venue data: a borrowed JSON string is never
/// `&'static`, so this impl does not apply to it and [`TradeId::new`] is the only way in.
impl From<&'static str> for TradeId {
    fn from(s: &'static str) -> Self {
        assert!(!s.is_empty(), "an empty trade_id literal is a source bug — see TradeId");
        Self(CompactString::const_new(s))
    }
}

/// Refuses an empty id on the way IN, so neither a hostile venue nor a legacy journal row can
/// smuggle back the value the type exists to forbid.
///
/// ⚠ A pre-existing journal/Parquet row whose `trade_id` is `""` therefore fails to deserialize
/// rather than loading as an un-dedupable fill. That is the intended direction — such a row is
/// exactly the garbage this change eliminates — but it means the READ path must degrade per row
/// instead of failing a whole batch; see `vike_report::journal_read`.
impl<'de> Deserialize<'de> for TradeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let cow = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        TradeId::new(cow.as_ref()).map_err(serde::de::Error::custom)
    }
}

/// Force the process-wide `ustr` string-interner table to initialize NOW, off the hot path.
///
/// The first `Ustr` construction anywhere in the process lazily builds the global intern table —
/// a measured **~3.2 ms one-time cost** (vike-core `runtime_latency`: hop #0 = 3.19 ms, every
/// later hop sub-µs). Binaries call this once at startup (the core does, in `spawn_core_multi`)
/// so the first *live* fill/quote never eats it. Idempotent and cheap after the first call.
pub fn prewarm_interner() {
    let _ = ustr::ustr("");
}

/// Closed-set strong type for `position_side` (the Nautilus model: a plain integer enum, not a
/// string). Wire form stays `"BOTH"/"LONG"/"SHORT"` via `rename_all`, so fixtures/journal are
/// byte-identical. `From<&str>`/`Display` bridge the ~all existing `"BOTH".into()` /
/// `.to_string()` construction+read sites so they compile unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum PositionSide {
    /// One-way / spot (Python `"BOTH"`).
    #[default]
    Both,
    Long,
    Short,
}

impl std::fmt::Display for PositionSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PositionSide::Both => "BOTH",
            PositionSide::Long => "LONG",
            PositionSide::Short => "SHORT",
        })
    }
}

impl From<&str> for PositionSide {
    fn from(s: &str) -> Self {
        match s {
            "LONG" => PositionSide::Long,
            "SHORT" => PositionSide::Short,
            _ => PositionSide::Both, // "BOTH" and legacy empty/unknown → net/spot
        }
    }
}

impl From<String> for PositionSide {
    fn from(s: String) -> Self {
        PositionSide::from(s.as_str())
    }
}

/// Closed-set strong type for `liquidity_side`. Wire form stays lowercase `"maker"`/`"taker"`;
/// the venue-not-surfaced case is the empty string `""` (FX / prediction venues often don't
/// report liquidity), so fixtures/journal stay byte-identical. Manual serde because the empty
/// `Unknown` variant can't be expressed with `rename_all`. `From<&str>`/`Display`/`is_maker`
/// bridge the existing construction (`"maker".into()`) and read (`== "maker"`) sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LiquiditySide {
    Maker,
    Taker,
    /// Venue did not surface liquidity — serializes as `""`.
    #[default]
    Unknown,
}

impl LiquiditySide {
    /// True only for a confirmed maker fill — the `liquidity_side == "maker"` twin.
    pub fn is_maker(self) -> bool {
        matches!(self, LiquiditySide::Maker)
    }
    fn as_wire(self) -> &'static str {
        match self {
            LiquiditySide::Maker => "maker",
            LiquiditySide::Taker => "taker",
            LiquiditySide::Unknown => "",
        }
    }
}

impl std::fmt::Display for LiquiditySide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl From<&str> for LiquiditySide {
    fn from(s: &str) -> Self {
        match s {
            "maker" => LiquiditySide::Maker,
            "taker" => LiquiditySide::Taker,
            _ => LiquiditySide::Unknown, // "" and any legacy/unknown label → not surfaced
        }
    }
}

impl From<String> for LiquiditySide {
    fn from(s: String) -> Self {
        LiquiditySide::from(s.as_str())
    }
}

impl Serialize for LiquiditySide {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for LiquiditySide {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let cow = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Ok(LiquiditySide::from(cow.as_ref()))
    }
}

/// One execution (partial or full). `mark_price` for perp funding/liq pricing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillEvent {
    /// Venue execution id — the dedup key across reconnects. This is vike's `ExecId`; it maps to
    /// FIX `ExecID(17)` but ONLY on Trade (fill) reports. FIX carries an ExecID on EVERY execution
    /// report (ack/cancel/reject/pending too), so no rename is needed here — instead a future FIX
    /// adapter reserves a report-level exec-id on the lifecycle events below (§7.4).
    pub trade_id: TradeId,
    pub client_order_id: String,
    pub venue: Ustr,
    pub symbol: Ustr,
    pub side: i32,
    pub last_qty: f64,
    pub last_px: f64,
    /// SIGNED: > 0 = charge/cost, < 0 = maker rebate/income (Account nets into balance)
    #[serde(default)]
    pub commission: f64,
    /// Fee currency of `commission` (e.g. "BNB", "USDT"), when the venue surfaces it. Empty =
    /// not surfaced (single-ccy or unknown venue) → the per-asset ledger is not attributed and
    /// behavior is byte-identical to before this field existed. `skip_serializing_if` keeps it
    /// out of the wire/journal/fixtures when empty, so r1/r5/r7 parity JSON gains no key.
    #[serde(default = "empty_ustr", skip_serializing_if = "ustr_is_empty")]
    pub commission_asset: Ustr,
    /// 'maker' | 'taker' | '' (venue did not surface it)
    #[serde(default)]
    pub liquidity_side: LiquiditySide,
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub mark_price: Option<f64>,
    /// 'BOTH' one-way/spot | 'LONG' | 'SHORT' (hedge perps)
    #[serde(default = "default_both")]
    pub position_side: PositionSide,
}

fn default_both() -> PositionSide {
    PositionSide::Both
}

/// serde helpers for `commission_asset: Ustr` — currency codes are a small, repeated set, so
/// interning is sound (bounded cardinality). Keep the "empty ⇒ omit from wire" behavior so
/// fixtures/journal that never surfaced a fee asset gain no key (byte-identical).
fn empty_ustr() -> Ustr {
    ustr::ustr("")
}
fn ustr_is_empty(u: &Ustr) -> bool {
    u.is_empty()
}

// --- order lifecycle events ---------------------------------------------------------------
// §7.4 FIX-prep, RESERVE when a FIX adapter lands (NOT now — it would add an unused `Option` to all
// ~51 event-construction sites for zero current value): an optional report-level `exec_id` here
// (FIX ExecID(17) on non-fill reports) + `orig_client_order_id` on OrderRequest (FIX OrigClOrdID(41),
// the cancel/replace modify chain). Crypto favors cancel+new, so neither is exercised yet.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderSubmitted {
    pub client_order_id: String,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderAccepted {
    pub client_order_id: String,
    #[serde(default)]
    pub venue_order_id: Option<CompactString>,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRejected {
    pub client_order_id: String,
    #[serde(default)]
    pub reason: CompactString,
    #[serde(default)]
    pub ts: i64,
}

/// Rust-native cancel-reject (no Python twin) — mirrors NautilusTrader's `OrderCancelRejected`.
/// A cancel request the venue/transport refused or could not deliver. NON-TERMINAL: the order
/// remains live and cancelable; this is an advisory so the failed intent is never swallowed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderCancelRejected {
    pub client_order_id: String,
    #[serde(default)]
    pub reason: CompactString,
    #[serde(default)]
    pub ts: i64,
}

/// Rust-native modify-reject (no Python twin) — mirrors NautilusTrader's `OrderModifyRejected`.
/// A modify request the venue/transport refused or could not deliver. NON-TERMINAL: the order
/// keeps its existing terms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderModifyRejected {
    pub client_order_id: String,
    #[serde(default)]
    pub reason: CompactString,
    #[serde(default)]
    pub ts: i64,
}

/// RiskGate veto (pre-venue) — an event, never a modal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderDenied {
    pub client_order_id: String,
    #[serde(default)]
    pub reason: CompactString,
    #[serde(default)]
    pub ts: i64,
}

/// A venue-side conditional (stop) fired — distinct from the subsequent fill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderTriggered {
    pub client_order_id: String,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderPartiallyFilled {
    pub client_order_id: String,
    pub fill: FillEvent,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderFilled {
    pub client_order_id: String,
    pub fill: FillEvent,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderCanceled {
    pub client_order_id: String,
    #[serde(default)]
    pub reason: CompactString,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderExpired {
    pub client_order_id: String,
    #[serde(default)]
    pub ts: i64,
}

/// Venue force-close of an order (perp liquidation) — FSM counterpart of PositionLiquidated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderLiquidated {
    pub client_order_id: String,
    #[serde(default)]
    pub liq_price: f64,
    #[serde(default)]
    pub ts: i64,
}

/// A resting order's quantity and/or price changed in place (venues call it modify or amend).
///
/// RUST-NATIVE extension — **no Python twin**. The oracle (`exec/events.py`) deliberately omits the
/// FIX cancel/replace chain (see the §7.4 note above: "Crypto favors cancel+new"); this is the HFT
/// track's true in-place modify. In the FSM it is a NON-TERMINAL self-transition: the order stays
/// `ACCEPTED`/`TRIGGERED`/`PARTIALLY_FILLED` with updated resting terms, so the "exactly one terminal
/// event per order" invariant is preserved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderModified {
    pub client_order_id: String,
    #[serde(default)]
    pub venue_order_id: Option<CompactString>,
    /// New TOTAL order quantity; `None` leaves quantity unchanged.
    #[serde(default)]
    pub new_qty: Option<f64>,
    /// New limit/trigger price; `None` leaves price unchanged.
    #[serde(default)]
    pub new_price: Option<f64>,
    #[serde(default)]
    pub ts: i64,
}

// --- position / account (derived from fills) -----------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionOpened {
    pub venue: Ustr,
    pub symbol: Ustr,
    /// 'BOTH' (one-way/spot) | 'LONG' | 'SHORT' (hedge perps)
    pub position_side: PositionSide,
    pub qty: f64,
    pub avg_px: f64,
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub mark_price: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionChanged {
    pub venue: Ustr,
    pub symbol: Ustr,
    pub position_side: PositionSide,
    pub qty: f64,
    pub avg_px: f64,
    #[serde(default)]
    pub realized_pnl: f64,
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub mark_price: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionClosed {
    pub venue: Ustr,
    pub symbol: Ustr,
    pub position_side: PositionSide,
    #[serde(default)]
    pub realized_pnl: f64,
    #[serde(default)]
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    pub venue: Ustr,
    /// (asset, qty) pairs — immutable
    #[serde(default)]
    pub balances: Vec<(String, f64)>,
    #[serde(default)]
    pub ts: i64,
    /// **WHICH ACCOUNT of [`Self::venue`] this balance snapshot belongs to** — the
    /// `vike_exec::ExecutionEngine::route_key` of the engine that must fold it, or `None` for the
    /// venue's sole/default account.
    ///
    /// ## Why this field exists at all
    ///
    /// Every other venue-tagged payload carries a CLIENT-ORDER-ID or a SYMBOL, and `vike_core`'s
    /// `route_event` disambiguates two accounts of one exchange with those — the coid exactly (it
    /// resolves through the submit-time `coid_venue` map, so it needs nothing on the wire), the
    /// symbol only while one engine of the venue claims it. An account-wide balance snapshot has
    /// NEITHER, so it had nothing to disambiguate on and folded into the venue's DEFAULT engine: a
    /// second account's balances landed in the first account's book, while its fills and positions
    /// routed correctly.
    ///
    /// ⚠ The symbol used to be described here as exact, because two active accounts of one venue
    /// could not be armed on one symbol. That refusal is GONE — two accounts on one instrument is
    /// an ordinary spread (`vike_config::venue_accounts`) — which does not weaken this field's
    /// case; it strengthens it, since this payload was never covered by the symbol either way.
    ///
    /// ## Who fills it — NOT the bridge
    ///
    /// A venue adapter knows nothing about accounts: it holds one credential set and emits the
    /// canonical venue id, so every bridge in this tree writes `None` here and none of them has to
    /// learn what an account label is. The MOUNT stamps it, in `vike_mount::account_event_sender`,
    /// because that is the one place a venue's account IDENTITY exists — and it stamps NOTHING for
    /// the default account (`vike_exec::EventSender::routed` skips a key equal to the payload's own
    /// venue), which is what keeps a single-account box byte-identical on the wire.
    ///
    /// ## Wire compatibility
    ///
    /// An older frame — every journal segment and every parity fixture on disk — reads back as
    /// `None`, meaning THE venue's sole account. ⚠ That property comes from the field being an
    /// `Option` (serde's missing-field handling answers `visit_none` for one), NOT from the
    /// `#[serde(default)]` below: a mutation test removing that attribute left every
    /// backward-read test GREEN. It is kept for the reason the sibling fields carry it — it says
    /// out loud that absence is legal — and the property is gated instead by this file's own
    /// `account_state_route_key_tests::pre_field_bytes_read_back_as_the_sole_account`, which a
    /// `default` naming a NON-`None` value does redden.
    ///
    /// `skip_serializing_if` is the load-bearing half: an unstamped snapshot serializes to EXACTLY
    /// the bytes it always did — no `route_key` key is emitted at all — so a default-account box's
    /// journal segments and every parity fixture are byte-for-byte unchanged. `Ustr` is sound here
    /// for the same bounded-cardinality reason `venue` is: the value set is the process's mounted
    /// accounts, a handful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_key: Option<Ustr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingEvent {
    pub venue: Ustr,
    pub symbol: Ustr,
    pub position_side: PositionSide,
    pub funding_rate: f64,
    pub amount: f64,
    #[serde(default)]
    pub mark_price: Option<f64>,
    #[serde(default)]
    pub ts: i64,
    /// **WHICH ACCOUNT of [`Self::venue`] paid or received this funding** — the
    /// `vike_exec::ExecutionEngine::route_key` of the engine that must fold it, or `None` for the
    /// venue's sole/default account. Stamped by the MOUNT, exactly like
    /// [`AccountState::route_key`]; see that field for who fills it, why no bridge has to learn
    /// what an account label is, and why `skip_serializing_if` keeps a default-account box's bytes
    /// unchanged.
    ///
    /// ⚠ **This payload has NO other routing handle, and that was missed once.** A funding payment
    /// names a symbol but no client-order-id, so `vike_core`'s `route_event` could only fall back
    /// to the symbol — and the symbol stopped being an account key the moment two accounts of one
    /// venue were allowed onto one instrument (`vike_config::venue_accounts`: two accounts on one
    /// instrument is an ordinary SPREAD, which is the whole point of the mount `account` field).
    /// With both engines claiming `BTCUSDT`, `engine_idx_for_venue_symbol` answers `None` and the
    /// venue lookup below it resolves the venue's DEFAULT engine — so a labelled account's funding
    /// debit landed on the default account's `balance`, and neither book was right afterwards. It
    /// is the same defect [`AccountState::route_key`] was added for, on the payload nobody checked
    /// because it happens to carry a symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_key: Option<Ustr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionLiquidated {
    pub venue: Ustr,
    pub symbol: Ustr,
    pub position_side: PositionSide,
    pub qty: f64,
    pub liq_price: f64,
    #[serde(default)]
    pub fee: f64,
    #[serde(default)]
    pub ts: i64,
    /// venue exec/trade id — the per-frame liquidation dedup key
    #[serde(default)]
    pub trade_id: CompactString,
    /// **WHICH ACCOUNT of [`Self::venue`] was liquidated** — the
    /// `vike_exec::ExecutionEngine::route_key` of the engine that must fold it, or `None` for the
    /// venue's sole/default account. Stamped by the MOUNT, exactly like
    /// [`AccountState::route_key`] and [`FundingEvent::route_key`].
    ///
    /// ⚠ It carries a symbol and NO client-order-id, so it is the second payload the symbol
    /// stopped disambiguating when two accounts of one venue were allowed onto one instrument —
    /// see [`FundingEvent::route_key`] for the argument in full. This is the more damaging of the
    /// two: `Account::apply_liquidation` CLOSES the position at the venue's liquidation price and
    /// books the realized PnL, so an unstamped frame flattened the DEFAULT account's book on the
    /// strength of a labelled account's liquidation while the account that was actually liquidated
    /// went on reporting its position open.
    ///
    /// ⚠ [`Self::trade_id`] is a DEDUP key and is not a substitute: `ExecutionEngine::seen_liq_ids`
    /// is per-engine, so two engines of one exchange would each accept the same frame once. The
    /// routing decision has to happen before the dedup, which is why it rides here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_key: Option<Ustr>,
}

/// The tagged union the core loop consumes (and fixtures replay).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    /// Bare execution-report lane (the Account fold). The wire tag stays `"FillEvent"` —
    /// fixtures and the exec_db journal pin it — only the Rust variant drops the suffix.
    #[serde(rename = "FillEvent")]
    Fill(FillEvent),
    OrderSubmitted(OrderSubmitted),
    OrderAccepted(OrderAccepted),
    OrderRejected(OrderRejected),
    OrderDenied(OrderDenied),
    OrderTriggered(OrderTriggered),
    OrderPartiallyFilled(OrderPartiallyFilled),
    OrderFilled(OrderFilled),
    OrderCanceled(OrderCanceled),
    OrderExpired(OrderExpired),
    OrderLiquidated(OrderLiquidated),
    /// Rust-native modify (no Python twin) — see [`OrderModified`].
    OrderModified(OrderModified),
    PositionOpened(PositionOpened),
    PositionChanged(PositionChanged),
    PositionClosed(PositionClosed),
    AccountState(AccountState),
    /// Wire tag stays `"FundingEvent"` (same rule as `Fill`).
    #[serde(rename = "FundingEvent")]
    Funding(FundingEvent),
    PositionLiquidated(PositionLiquidated),
    /// Rust-native cancel-reject (no Python twin) — mirrors NautilusTrader's
    /// `OrderCancelRejected`: a venue/transport failure of a cancel request. NON-TERMINAL —
    /// the order stays live (see `ManagedOrder::apply`). Emitted where a failed cancel would
    /// otherwise be swallowed.
    OrderCancelRejected(OrderCancelRejected),
    /// Rust-native modify-reject (no Python twin) — mirrors NautilusTrader's
    /// `OrderModifyRejected`: a venue/transport failure of a modify request. NON-TERMINAL —
    /// the order keeps its terms.
    OrderModifyRejected(OrderModifyRejected),
}

#[cfg(test)]
mod reject_event_tests {
    use super::*;

    #[test]
    fn order_cancel_rejected_round_trips_with_wire_tag() {
        let ev = Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: "c1".into(),
            reason: "network error: timed out".into(),
            ts: 42,
        });
        let json = serde_json::to_string(&ev).expect("serialize");
        assert!(
            json.contains("\"type\":\"OrderCancelRejected\""),
            "wire tag must be OrderCancelRejected, got {json}"
        );
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn order_modify_rejected_round_trips_with_wire_tag() {
        let ev = Event::OrderModifyRejected(OrderModifyRejected {
            client_order_id: "c2".into(),
            reason: "modify rejected".into(),
            ts: 7,
        });
        let json = serde_json::to_string(&ev).expect("serialize");
        assert!(
            json.contains("\"type\":\"OrderModifyRejected\""),
            "wire tag must be OrderModifyRejected, got {json}"
        );
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    /// LiquiditySide must be byte-identical to the old `String` field: lowercase maker/taker,
    /// empty string for the unsurfaced case, and any unknown/legacy label folds to Unknown.
    #[test]
    fn liquidity_side_wire_is_byte_identical() {
        for (variant, wire) in [
            (LiquiditySide::Maker, "\"maker\""),
            (LiquiditySide::Taker, "\"taker\""),
            (LiquiditySide::Unknown, "\"\""),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
            assert_eq!(serde_json::from_str::<LiquiditySide>(wire).unwrap(), variant);
        }
        // legacy/unknown labels and a missing field both fold to Unknown (the old "" default)
        assert_eq!(
            serde_json::from_str::<LiquiditySide>("\"foo\"").unwrap(),
            LiquiditySide::Unknown
        );
        assert_eq!(LiquiditySide::default(), LiquiditySide::Unknown);
    }
}

/// The WIRE half of [`AccountState::route_key`] — the field a second venue account needs and a
/// single-account box must not be able to tell exists.
///
/// Three properties, and the first is the one the whole design rests on: an UNSTAMPED snapshot
/// serializes to the exact bytes it serialized to before the field was added. Everything a
/// default-account box writes — its journal segments, its fixtures, its captures — is therefore
/// byte-for-byte unchanged, which is why the mount can stamp unconditionally instead of branching.
#[cfg(test)]
mod account_state_route_key_tests {
    use super::*;

    /// THE PIN: the exact JSON an unstamped `AccountState` produced BEFORE `route_key` existed,
    /// spelled as a literal rather than derived from the type, so a change to the type cannot move
    /// both sides together. (That failure mode is not hypothetical in this workspace — a fixture
    /// seeded through the very function under test makes an equality between two identical wrong
    /// answers pass.)
    const PRE_FIELD_WIRE: &str = concat!(
        r#"{"type":"AccountState","venue":"binance","#,
        r#""balances":[["USDT",1234.5]],"ts":7}"#
    );

    fn unstamped() -> Event {
        Event::AccountState(AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 1234.5)],
            ts: 7,
            route_key: None,
        })
    }

    #[test]
    fn an_unstamped_snapshot_is_byte_identical_to_the_pre_field_wire() {
        assert_eq!(
            serde_json::to_string(&unstamped()).expect("serialize"),
            PRE_FIELD_WIRE,
            "a default-account box must emit no `route_key` key at all — journal bytes, fixtures \
             and captures all depend on it"
        );
    }

    /// The READ direction of the same property: bytes written by a build that had no such field
    /// (every journal segment and fixture on disk today) parse, and mean "the venue's sole
    /// account" rather than erroring or inventing a key.
    #[test]
    fn pre_field_bytes_read_back_as_the_sole_account() {
        let back: Event = serde_json::from_str(PRE_FIELD_WIRE).expect("deserialize");
        assert_eq!(back, unstamped());
        match back {
            Event::AccountState(a) => assert_eq!(a.route_key, None),
            other => panic!("expected AccountState, got {other:?}"),
        }
    }

    /// …and a STAMPED one carries the key, round-trips, and is NOT equal to the unstamped twin —
    /// the property `vike_core`'s router reads.
    #[test]
    fn a_stamped_snapshot_carries_its_route_key_and_round_trips() {
        let ev = Event::AccountState(AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 1234.5)],
            ts: 7,
            route_key: Some("binance#alt".into()),
        });
        let json = serde_json::to_string(&ev).expect("serialize");
        assert!(json.contains(r#""route_key":"binance#alt""#), "got {json}");
        assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), ev);
        assert_ne!(ev, unstamped(), "the stamp must be observable, not cosmetic");
    }
}

/// The wire-compatibility twin of [`account_state_route_key_tests`], for the OTHER two coid-less
/// venue-tagged payloads. They were left unstamped on the argument that they carry a symbol and the
/// symbol is an exact account key; it stopped being one when two accounts of a venue were allowed
/// onto one instrument, so they carry the same field now — and the same byte-identity obligation,
/// since both appear in journal segments and in captured fixtures.
#[cfg(test)]
mod funding_and_liquidation_route_key_tests {
    use super::*;

    /// THE PINS: the exact JSON each payload produced BEFORE `route_key` existed, spelled as
    /// literals for the reason the `AccountState` twin gives — a fixture derived from the type
    /// under test moves both sides together and proves nothing.
    const PRE_FIELD_FUNDING: &str = concat!(
        r#"{"type":"FundingEvent","venue":"binance","symbol":"BTCUSDT","#,
        r#""position_side":"BOTH","funding_rate":0.0001,"amount":-1.25,"#,
        r#""mark_price":null,"ts":7}"#
    );

    const PRE_FIELD_LIQ: &str = concat!(
        r#"{"type":"PositionLiquidated","venue":"binance","symbol":"BTCUSDT","#,
        r#""position_side":"BOTH","qty":1.0,"liq_price":90.0,"fee":0.2,"ts":7,"#,
        r#""trade_id":"l1"}"#
    );

    fn unstamped_funding() -> Event {
        Event::Funding(FundingEvent {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: -1.25,
            mark_price: None,
            ts: 7,
            route_key: None,
        })
    }

    fn unstamped_liquidation() -> Event {
        Event::PositionLiquidated(PositionLiquidated {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 1.0,
            liq_price: 90.0,
            fee: 0.2,
            ts: 7,
            trade_id: "l1".into(),
            route_key: None,
        })
    }

    #[test]
    fn an_unstamped_payload_is_byte_identical_to_the_pre_field_wire() {
        assert_eq!(
            serde_json::to_string(&unstamped_funding()).expect("serialize"),
            PRE_FIELD_FUNDING,
            "a default-account box must emit no `route_key` key at all"
        );
        assert_eq!(
            serde_json::to_string(&unstamped_liquidation()).expect("serialize"),
            PRE_FIELD_LIQ,
            "a default-account box must emit no `route_key` key at all"
        );
    }

    /// The READ direction: bytes written by a build that had no such field — every journal segment
    /// and every captured fixture on disk today — parse, and mean "the venue's sole account".
    #[test]
    fn pre_field_bytes_read_back_as_the_sole_account() {
        assert_eq!(
            serde_json::from_str::<Event>(PRE_FIELD_FUNDING).expect("deserialize"),
            unstamped_funding()
        );
        assert_eq!(
            serde_json::from_str::<Event>(PRE_FIELD_LIQ).expect("deserialize"),
            unstamped_liquidation()
        );
    }

    /// …and a STAMPED one carries the key, round-trips, and is NOT equal to its unstamped twin —
    /// the property `vike_core`'s `route_event` and `ExecutionEngine::on_event` both read.
    #[test]
    fn a_stamped_payload_carries_its_route_key_and_round_trips() {
        for (unstamped, stamped) in [
            (unstamped_funding(), {
                let mut e = unstamped_funding();
                if let Event::Funding(f) = &mut e {
                    f.route_key = Some("binance#alt".into());
                }
                e
            }),
            (unstamped_liquidation(), {
                let mut e = unstamped_liquidation();
                if let Event::PositionLiquidated(p) = &mut e {
                    p.route_key = Some("binance#alt".into());
                }
                e
            }),
        ] {
            let json = serde_json::to_string(&stamped).expect("serialize");
            assert!(json.contains(r#""route_key":"binance#alt""#), "got {json}");
            assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), stamped);
            assert_ne!(stamped, unstamped, "the stamp must be observable, not cosmetic");
        }
    }
}
