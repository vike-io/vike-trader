//! The node wire value types: a STANDALONE serde mirror of the rendered core snapshot
//! ([`WireSnapshot`]) and of the order-write command vocabulary ([`WireCommand`]); the account
//! admin plane (decision 0065) lives in the private `account` child, re-exported here by name.
//!
//! # Why standalone mirrors (not the core types)
//!
//! These re-declare the field shapes of `vike_exec::{CoreSnapshot, OrderView, PositionView,
//! TradingState}`, `vike_exec::{Command, OrderIntent}` and `vike_model::OrderRequest` instead of
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
    WireDirectory, WireDirectoryAccount, WireDirectoryVenue,
};

/// The account trading state — mirrors `vike_exec::TradingState` (same variant names, so a JSON
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
    /// predates the field. The tell: [`WireVenueBlock::mode`] shipped in the same release, so
    /// blocks carrying a `mode` (PRESENCE, not value) mean orders name their account; otherwise,
    /// on a venue with several blocks, the order's account is UNKNOWN — never the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub symbol: String,
    /// +1 buy / -1 sell.
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    /// Rendered order status (e.g. `"Accepted"`, `"Filled"`).
    pub status: String,
    pub venue_order_id: Option<String>,
    pub filled_qty: f64,
    pub avg_fill_px: f64,
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

/// What stands behind an account's orders — mirrors `vike_exec::EngineMode`.
///
/// ⚠ **A word added later is SAFE for an older reader, not free**: it reads `None` and keeps its
/// link ([`WireVenueBlock::mode`]) but cannot NAME the mode. A mode whose absence would mislead
/// needs a new FIELD or a `NODE_PROTO_VERSION` bump read from the client's `Hello` — never a
/// `Welcome.features` word (the NODE advertises those; they say nothing of what readers know).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireEngineMode {
    /// A simulated book: no order leaves the process.
    ///
    /// ⚠ That is what the word MEANS, not what its presence proves: `Paper` is also an engine's
    /// seed (`vike_exec::EngineMode`), so an engine restored around a real client may publish it
    /// too — `vike_exec::VenueBlock::mode` carries the argument.
    Paper,
    /// A venue client on the venue's demo, testnet or sandbox account.
    Demo,
    /// A venue client on a real-money account.
    Live,
}

/// [`WireVenueBlock::mode`]'s reader: the three known words are `Some`; any other value or shape,
/// `null` and an absent key are `None`. ⚠ Not the derived reader: that fails the WHOLE frame on an
/// unknown word, and the desktop's observe loop ends the link on an undecodable frame — so a newer
/// node's mode word would silently drop every older desktop. Serialisation is unchanged.
fn engine_mode_or_not_said<'de, D>(deserializer: D) -> Result<Option<WireEngineMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| serde_json::from_value::<WireEngineMode>(value).ok()))
}

/// A per-venue ledger block — mirrors the display fields of `vike_exec::VenueBlock`; the heavy
/// internals (multiplier grid, fee schedule, margin math) stay core-side.
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
    /// What stands behind this account's orders. ⚠ `None` = the node did not say (also for an
    /// unknown word, [`WireEngineMode`]): render unknown, never guess. `Some(Paper)` proves nothing
    /// ([`WireEngineMode::Paper`]). ⚠ Its PRESENCE is the tell that this node names each ORDER's
    /// account ([`WireOrderView::account`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "engine_mode_or_not_said"
    )]
    pub mode: Option<WireEngineMode>,
}

/// One OHLCV candle — mirrors the display subset of `vike_model::Bar`. Short field names keep the
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
    /// Cross-venue total equity (the core's `py_sum` `equity_total`).
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
    /// Which daemon this is ([`WireNodeIdentity`]); `None` from a node that predates the field.
    #[serde(default)]
    pub identity: Option<WireNodeIdentity>,
    /// A digest of the node's MOUNTED ACCOUNT SET (`vike_exec::CoreSnapshot::accounts_epoch`),
    /// changing exactly when the set of route keys does. `0` = predates the field, or no accounts.
    ///
    /// ⚠ It answers ONE question: did the set move between a preview and its confirm? A client
    /// stamps it into the preview and refuses a stale confirm (*"the node's account set changed
    /// since your preview, re-preview"*). A DIGEST, not a counter, which would restart at zero with
    /// the daemon (`vike_core::snapshot::accounts_epoch_of`).
    #[serde(default)]
    pub accounts_epoch: u64,
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
        }
    }
}

/// WHICH daemon a [`WireSnapshot`] describes — name, mounted strategy, paper-vs-live, build — so a
/// GUI holding several backends can label them. Additive (the module's additive-field contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireNodeIdentity {
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
    /// `vike_mount::build_node`'s `live_venues` (a real `ExecutionClient` was built), not the
    /// daemon's gate: a mount without credentials, `data_only`, or demoted to paper reads `false`
    /// while [`WireNodeIdentity::live`] reads `true` — the normal, informative disagreement.
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
    pub identity: WireNodeIdentity,
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
    /// The CLI's READ cell: the consuming binary's short name, `"yes"` (a library reads it), or
    /// `"NO"` (nothing reads it: a configured row with this cell is a misconfiguration).
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
    /// `"market"` | `"limit"` | `"stop"` | `"take_profit"`.
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
/// ⚠ **No account and no client order id, because `BracketSpec` has neither.** A node accepts a
/// bracket only on the default account of a venue with exactly ONE engine, for a symbol that
/// engine trades, and on binance and aster only when it is a PERP; it also refuses a side other
/// than ±1, non-finite or non-positive sizes/prices and exits on the wrong side, all before its
/// `Ack` (`vike-tradehub`'s `account_refusal` and `bracket_refusal`). The core mints the three ids
/// after the `Ack`; the entry id reaches the client as each held exit's `parent_order_id`.
///
/// ⚠ **`deny_unknown_fields` is load-bearing**: when a bracket learns its account, an older node
/// must ERROR on that key rather than drop it and route to the default book. Never relax it to
/// add a field; a new field rides a new `Welcome.features` word.
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
}

/// The command vocabulary a [`Scope::Write`](crate::proto::Scope) client may issue — a mirror of
/// the SESSION-relevant verbs in `vike_exec::{Command, OrderIntent}` (orders, brackets, cancels,
/// flatten, trading state, strategy params/mounts, settings writes). NOT the reconcile/journal
/// surface (`ApplySnapshot`, `ReconcileReports`, `ConfirmRecon`, …): that is core-internal
/// plumbing and never a remote client's business. A server lowers each variant into the core's
/// `Command`/`OrderIntent` at its edge.
///
/// ⚠ `Debug` stays DERIVED: no variant may carry a credential value (0065 §4.1). The account verbs
/// that do are [`AccountRequest`], outside this enum, with a redacting `Debug`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireCommand {
    /// Submit one order (`OrderIntent::Submit`).
    Submit(WireOrderRequest),
    /// Submit one TP/SL BRACKET (`OrderIntent::Bracket`). DEFAULT ACCOUNT ONLY — see
    /// [`WireBracketSpec`]. Requires [`crate::proto::FEATURE_BRACKET`] (refused client-side
    /// otherwise).
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
            // `Some(v)` = every account of that exchange (`CoreThread::exit_scope_engines`);
            // `None` = every engine, never refused.
            WireCommand::MassCancel { venue, .. } => venue.as_deref(),
            WireCommand::MarketExit { venue, .. } => venue.as_deref(),
            WireCommand::Cancel(_)
            | WireCommand::Modify { .. }
            | WireCommand::SetTradingState(_)
            | WireCommand::UnmountStrategy { .. }
            | WireCommand::SetSetting { .. } => None,
        }
    }
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;

#[path = "the_account_field_is_invisible_until_it_is_used.rs"]
#[cfg(test)]
mod the_account_field_is_invisible_until_it_is_used;
