//! The node wire value types: a STANDALONE serde mirror of the rendered core snapshot
//! ([`WireSnapshot`]) and of the order-write command vocabulary ([`WireCommand`]).
//!
//! # Why standalone mirrors (not the core types)
//!
//! These deliberately re-declare the field shapes of `vike_core::snapshot::{CoreSnapshot, OrderView,
//! PositionView}`, `vike_exec::TradingState`, and `vike_exec::{Command, OrderIntent}` /
//! `vike_model::OrderRequest` rather than depending on `vike-core`/`vike-exec`/`vike-model`. Reasons:
//!
//! - **Independence / weight.** Depending on `vike-core` would pull the whole live-runtime tree
//!   (tokio runtime, the execution engines) into what is meant to be the LIGHT thin-client wire
//!   crate. The mirror keeps this crate's dependency graph to serde + the framing + the auth crypto.
//! - **A stable wire contract.** The rendered snapshot the GUI needs is a small, flat projection;
//!   pinning it here means an internal refactor of `CoreSnapshot` cannot silently change the node
//!   wire schema. The mirror is the schema; a server binding this proto is responsible for the
//!   `CoreSnapshot -> WireSnapshot` projection at its edge (a later PR).
//!
//! The tradeoff: a field added to `CoreSnapshot` that the GUI must see has to be added here too.
//! That is the intended seam — the wire schema evolves on purpose, versioned by
//! [`crate::proto::NODE_PROTO_VERSION`].

use serde::{Deserialize, Serialize};

/// The account trading state — a standalone mirror of `vike_exec::risk::TradingState` (same three
/// variants, same names, so a string/JSON round-trip matches the core enum's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireTradingState {
    /// Normal trading.
    Active,
    /// Only position-reducing orders allowed.
    Reducing,
    /// No new orders (kill switch).
    Halted,
}

/// One open/closed order — mirrors `vike_core::snapshot::OrderView`. `status` is carried as the
/// rendered string (`format!("{:?}", OrderStatus)` at the projecting edge) so this crate need not
/// mirror the `OrderStatus` enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireOrderView {
    pub client_order_id: String,
    pub venue: String,
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

/// One net/hedge position leg — mirrors `vike_core::snapshot::PositionView` (the display-relevant
/// subset). `position_side` is the leg label (`"BOTH"`/`"long"`/`"short"`).
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

/// One PENDING bracket exit the live runtime is HOLDING off the venue until its OTO parent fills —
/// mirrors `vike_core::snapshot::HeldOrderView`. These are NOT in [`WireSnapshot::orders`] (they have
/// no registered/venue order yet); `parent_order_id` names the entry whose fill releases the leg.
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

/// A per-venue ledger block — mirrors the display-relevant fields of `vike_core::snapshot::
/// VenueBlock`. The heavy internals (the `Arc` multiplier grid, fee schedule, margin math) stay
/// core-side; the node projects the numbers a GUI renders.
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
    /// **WHICH ACCOUNT of `venue` this block is** — absent for the unlabelled one, the convention
    /// `vike_model::account_keys::AccountLabel` states everywhere it appears. The wire mirror of
    /// `vike_core::snapshot::VenueBlock::account`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// **This engine's own ROUTING key** — the bare venue id for the default account, `venue#LABEL`
    /// for a labelled one.
    ///
    /// ⚠ **THE GATE COMPARES AGAINST THIS, not against [`Self::venue`]**, and that is
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`'s verdict 2: the
    /// check must ask the ROUTING question and not a neighbouring one, because a refusal looser
    /// than the routing *"admits precisely the strings the routing silently redirects."* Two
    /// accounts of one venue publish two identical `venue` strings and two DIFFERENT route keys, so
    /// `venue` cannot tell them apart and this can.
    ///
    /// `#[serde(default)]` for the reason every additive wire field carries one: an old node omits
    /// it, and a consumer must read that as *this node predates account routing* rather than as a
    /// decode failure. A single-account node's key EQUALS its venue, so a consumer that compares
    /// against either reads the same answer on every node in production today.
    #[serde(default)]
    pub route_key: String,
}

/// One OHLCV candle — a standalone mirror of the display subset of `vike_model::Bar` (kept free of a
/// `vike-model` dep so this crate stays light + DataFusion-free, exactly like `WireOrderView`). Short
/// field names keep the on-wire body small (a series carries up to ~300 of these).
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

/// One bar series for a `(venue, symbol, interval)` — the bounded tail the node publishes so an
/// observer can render a chart. `closed` is the LAST-K closed bars (capped NODE-side at the projection
/// edge — the core's own bar vec is unbounded); `forming` is the live in-progress candle (animates the
/// last bar). Standalone mirror of `vike_exec::BarSeries`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireBarSeries {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub closed: Vec<WireBar>,
    pub forming: Option<WireBar>,
}

/// The rendered core state view the node publishes — a standalone mirror of the display-relevant
/// projection of `vike_core::snapshot::CoreSnapshot`. `seq` increments per publish (change
/// detection), exactly as the core's does.
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
    /// Cross-venue total equity (the `py_sum` aggregate the core publishes as `equity_total`).
    pub equity_total: f64,
    /// Per-venue ledger blocks (primary first, then extras in registration order).
    pub venues: Vec<WireVenueBlock>,
    /// Order registry (insertion order), spanning every engine.
    pub orders: Vec<WireOrderView>,
    /// Top-level (primary-venue) positions — the documented mirror of `venues[0].positions`.
    pub positions: Vec<WirePositionView>,
    /// Pending bracket exits held off the venue until their OTO parent fills; empty on non-bracket
    /// runs.
    pub held_exits: Vec<WireHeldOrderView>,
    /// Recent delivered exec events (the bounded journal tail).
    pub recent_events: Vec<String>,
    /// Set once a handler panicked — the core is HALTED in safe-state.
    pub fault: Option<String>,
    /// Bounded bar tails (per mounted series) so an observer can render a chart — the LAST-K closed
    /// bars + the forming candle, projected node-side (see the node's publisher). Empty when the node
    /// has no bars yet, or from an older node that predates this field.
    #[serde(default)]
    pub bars: Vec<WireBarSeries>,
    /// The node's identity block (see [`WireNodeIdentity`]) — which daemon, which strategy,
    /// paper-vs-live, which build. `None` from an older node that predates this field.
    #[serde(default)]
    pub identity: Option<WireNodeIdentity>,
    /// **A digest of this node's MOUNTED ACCOUNT SET** — `vike_core::CoreSnapshot::accounts_epoch`,
    /// which changes whenever the set of route keys changes and not otherwise.
    ///
    /// ⚠ **It exists to answer ONE question: did the account set move between a preview and the
    /// confirm that follows it?** A client stamps the value it saw into the preview it issued, and
    /// refuses a confirm carrying a stale one — *"the node's account set changed since your
    /// preview, re-preview"*. Without it a two-call gate proves only that a preview happened, never
    /// that it described the node the write will reach.
    ///
    /// ⚠ A DIGEST rather than a counter, and `vike_core::CoreSnapshot::accounts_epoch_of` argues
    /// why the hash is spelled out rather than taken from `std::hash`: a counter restarts at zero
    /// when the daemon does, so a restart between preview and confirm would read as *unchanged*
    /// exactly when the set is most likely to have moved.
    ///
    /// `0` from a node that predates the field — and from a node with no accounts at all, which is
    /// the same answer for the same reason: nothing to have changed.
    #[serde(default)]
    pub accounts_epoch: u64,
}

impl WireSnapshot {
    /// An empty placeholder (`seq: 0`, no venues/orders/positions) — the wire twin of
    /// `vike_core::snapshot::CoreSnapshot::empty`. [`crate::remote_handle::RemoteCoreHandle::snapshot`]
    /// returns this before the node has pushed its first real frame, so a reader always has a value to
    /// render (the "connecting" state) instead of blocking or unwrapping a `None`.
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
            // No node, so no accounts — the same answer a node with none gives, for the same
            // reason: nothing to have changed since.
            accounts_epoch: 0,
        }
    }
}

/// WHICH daemon a [`WireSnapshot`] describes — name, mounted strategy, paper-vs-live, build
/// (split-plane spec, B3). Added so a GUI holding several backends can label them and tell paper
/// from live; without it two daemons' snapshots are indistinguishable on screen. Additive +
/// `#[serde(default)]` on the snapshot field — the same wire-evolution contract as
/// [`WireSnapshot::bars`], so it rides WITHOUT a `NODE_PROTO_VERSION` bump (the version is folded
/// into the signed auth message; bumping it would fail the handshake against every running node).
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
    /// **WHICH BOX the daemon is running on, as the daemon itself answers it** — the ONE fact in
    /// this block that the client cannot derive and had no other route to.
    ///
    /// Empty (`""`) means the daemon reported none: an older node that predates this field, a box
    /// whose kernel had no route to name a source address from (a loopback-only container), or an
    /// operator who left `config.toml`'s `tradehub_advertise_addr` unset on such a box. Empty is
    /// therefore NOT a fact about the daemon and a client must render it as "nothing was said",
    /// never as "the daemon is at 127.0.0.1".
    ///
    /// # ⚠ Why the daemon has to be the one to say it
    ///
    /// Both production listeners bind loopback, so an SSH tunnel is the only route in and the
    /// CLIENT side of the socket is always the tunnel mouth. Every thin client on every box
    /// therefore reads `127.0.0.1:7879` for its dial address, whichever daemon it is attached to.
    /// The client cannot discover the answer from the connection it holds; this daemon can, and it
    /// is already sending a frame.
    ///
    /// # ⚠ It is a CLAIM, not a verified fact
    ///
    /// Nothing on the client side checks it — a daemon could report anything, and the value it
    /// reports for itself is not necessarily reachable from the client (a loopback-bound daemon
    /// names a box, not a dialable endpoint). Render it as the daemon's own report.
    /// `crates/vike-tradehub/src/self_address.rs` is where it is produced and argues every case the
    /// route lookup can be wrong about; `vike_app_core::backend::backend_identity` is where it is rendered.
    ///
    /// Additive + `#[serde(default)]`, riding the [`WireSnapshot::bars`] wire-evolution contract:
    /// an old node's identity block carries no such key and parses as empty rather than erroring,
    /// so no `NODE_PROTO_VERSION` bump (the version is folded into the signed auth message, and a
    /// bump would fail the handshake against every running node).
    #[serde(default)]
    pub advertise_addr: String,
}

/// One mounted strategy, as the node reports it in a [`WireStrategyStatus`] (split-plane B4).
/// A single-mount daemon carries one row; a `[[mounts]]` daemon (I10, the static multi-mount)
/// carries one row per mount, in mount order — a longer `Vec` of the same struct is not a schema
/// change, which is exactly why the row was designed to ride in a `Vec`. A future field lands
/// additively (`#[serde(default)]`, the [`WireSnapshot::bars`] contract).
///
/// The mount's ADDRESSING KEY (`venue`/`symbol`/`interval` — what a [`WireCommand::UpdateParams`]
/// targets) was deliberately deferred here until a client needed to ADDRESS a row, on the grounds
/// that inventing a second structured copy of it before then would be a guess. **That condition is
/// now met** — the live-params read-modify-write patches a row and sends it back — so the key is on
/// the row, and the only prior route to it (string-parsing the `venue=… symbol=… interval=… ::`
/// prefix `vike-tradehub`'s `mounts_wire_params` prepends) stops being anybody's answer.
///
/// ⚠ Every field below `live` is `#[serde(default)]` and reads EMPTY from a node that predates
/// [`crate::proto::FEATURE_STRATEGY_PARAMS`]. That is indistinguishable from an honest "this mount
/// publishes no typed params", so **check the capability, never the emptiness** — the reason that
/// string exists as its own capability rather than riding `strategy-verbs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireMountRow {
    /// The mounted strategy's registry name (e.g. `"spread_maker"`).
    pub strategy: String,
    /// The mount's effective params, rendered (`DaemonProfile::effective_params`).
    ///
    /// PROSE, and boot-time: it is rendered from the daemon's process-static identity block, so it
    /// is stale the moment the first `UpdateParams` lands. [`Self::typed_params`] is the field a
    /// read-modify-write reads; this one stays for the human table and for the old-node fallback.
    pub params: String,
    /// True iff this mount trades LIVE; false = paper.
    ///
    /// ⚠ A per-VENUE fact, not the daemon's gate. `vike-tradehub` fills it from
    /// `vike_run::build_node`'s `live_venues` arming record — the set a venue enters exactly when a
    /// real `ExecutionClient` was constructed for it — so a mount whose venue holds no credentials,
    /// one declared `data_only = true`, and one whose synchronous connect demoted it to paper all
    /// read `false` here while [`WireNodeIdentity::live`] (the process-wide `flags.tradehub_live`
    /// gate) still reads `true`. The two DISAGREEING is the normal, informative case: the daemon is
    /// armed, this mount is not.
    pub live: bool,
    /// This mount's VENUE — the first third of the addressing key a [`WireCommand::UpdateParams`]
    /// targets. `""` from a node that predates [`crate::proto::FEATURE_STRATEGY_PARAMS`]; check
    /// the capability, never the emptiness.
    #[serde(default)]
    pub venue: String,
    /// This mount's SYMBOL — see [`Self::venue`].
    #[serde(default)]
    pub symbol: String,
    /// This mount's INTERVAL — see [`Self::venue`]. Part of the key because two mounts may differ
    /// only by it, which is exactly when a client must not guess.
    #[serde(default)]
    pub interval: String,
    /// The mount's LIVE tunables as the core's OWN `vike_model::StrategyParams` serde JSON — the
    /// exact shape [`WireCommand::UpdateParams`] takes back, so a client round-trips bytes it never
    /// has to understand. Carried as a [`serde_json::Value`] and NOT re-declared here, the same
    /// delegation `WireCommand::UpdateParams`' own doc argues for: the union grows a variant per
    /// strategy family, its JSON form is already a persisted journal schema, and a hand mirror here
    /// would drift within a release.
    ///
    /// Read off the LIVE core's published snapshot, not the daemon's boot block, so it reflects
    /// every re-tune that has landed. `None` means EITHER this mount's strategy publishes no typed
    /// params (`vike_model::Strategy::params`' default — in which case `UpdateParams` cannot
    /// address it either, so the read and the write have one domain) OR the node predates the
    /// capability; the two are told apart by `Welcome.features`, never by looking at this field.
    #[serde(default)]
    pub typed_params: Option<serde_json::Value>,
    /// **WHAT PRODUCT this mount trades**, as the stored word of a `vike_model::AssetClass` —
    /// `"CryptoPerp"`, `"Equity"`, `"PredictionMarket"`, and so on.
    ///
    /// ⚠ A WORD rather than the type, deliberately: this crate takes no `vike-model` dependency and
    /// is not going to grow one for a display field. The word is
    /// `vike_model::AssetClass::sql_word`, which that type's own `sql_word_is_the_serde_word` pins
    /// equal to its serde spelling — so the database cell, the settings row and this field are ONE
    /// spelling, and a client that wants the type parses it with `AssetClass::from_sql_word`.
    ///
    /// ⚠ `None` has THREE meanings and only [`crate::proto::FEATURE_MOUNT_CLASS`] tells them apart.
    /// Two of them are indistinguishable by looking: a node predating that capability, and a
    /// current node whose mount is still TOML-backed rather than row-backed (where
    /// `vike_tradehub::config::MountCfg`'s `asset_class` is legitimately absent — that asymmetry is
    /// the migration `docs/decisions/0061-an-instrument-names-its-kind.md` Phase 5 describes). A
    /// reader that checked the field instead of the capability would report the second as the first
    /// and send an operator to upgrade a daemon that is already current. Check the capability.
    ///
    /// ⚠ DISPLAY ONLY. 0061 is explicit that the exec plane infers nothing from a mount's class:
    /// this says what the mount IS, it is not a routing input, and a client may not decide with it.
    #[serde(default)]
    pub asset_class: Option<String>,
}

/// The payload of a `Response::StrategyStatus` (split-plane B4, the STRATEGY-level read verb): the
/// node's identity block plus its mounted strategies. `effective_params` is the daemon's ONE
/// resolved-params line (`DaemonProfile::effective_params` — the same string the identity block
/// carries), kept as its own field so a client that only wants "what is this node running" never
/// digs through the mounts `Vec`; per-mount params live on each [`WireMountRow`].
///
/// ⚠ `Eq` is deliberately ABSENT from this derive and from [`WireMountRow`]'s: that row now holds a
/// [`serde_json::Value`], which is not `Eq`. Zero wire effect, and the precedent is in this file —
/// [`WireCommand`] has derived only `PartialEq` for exactly this reason since it grew its own
/// `Value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireStrategyStatus {
    /// WHICH daemon answered — the same identity block every [`WireSnapshot`] carries.
    pub identity: WireNodeIdentity,
    /// The daemon's resolved effective-params line (`DaemonProfile::effective_params`).
    pub effective_params: String,
    /// One row per mounted strategy — exactly one today (see [`WireMountRow`] for the
    /// multi-mount forward design).
    pub mounts: Vec<WireMountRow>,
}

/// One of the node's effective settings-file rows (split-plane REQ-7, the read half) — the wire
/// rendering of `vike_config::show`'s `FileRow`, the SAME rows `vike-cli config show`'s files
/// table prints. Deliberately STRINGLY: every cell arrives rendered (and secret-shaped values
/// arrive already redacted — redaction happens in the shared builder, on construction, so no
/// serializer here could leak one), and this light client crate takes no `vike-config` dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSettingsRow {
    /// The settings SECTION this key belongs to (`"policy"`, `"config"`, …) — the CLI's grouping,
    /// and the key's own first segment. ⚠ It carried the settings FILE name (`"policy.toml"`)
    /// until `docs/decisions/0086` retired the files, so an older node may still send one. The
    /// WRITE half names a row by its `key` alone (`WireCommand::SetSetting`).
    pub section: String,
    /// The dotted key (`"config.tradehub_addr"`); its first segment is the settings section.
    pub key: String,
    /// The EFFECTIVE value, rendered; `""` = unset. Already redacted for a secret-shaped key.
    pub value: String,
    /// The layer that set it, as the CLI's one ORIGIN cell: `"default"`, `"db"` (a row of the
    /// settings database), `"env:VIKE_RECONCILE"`.
    pub origin: String,
    /// The CLI's READ cell: the consuming BINARY's short name, `"yes"` (a library reads it —
    /// every binary linking it), or `"NO"` (nothing reads it; a configured row with this cell is
    /// a misconfiguration, not a setting in force).
    pub read_by: String,
}

/// The payload of a `Response::SettingsShow` (split-plane REQ-7, the read half): the node's
/// effective settings-file rows, rendered by the node from the same `vike_config::show` builder
/// `vike-cli config show` uses — no second vocabulary, no second redaction rule.
///
/// The FILES half only, on purpose: the CLI's env-registry half (several hundred rows whose
/// credential grid discloses `<set>`/`<unset>` per venue key) stays a local-box disclosure — see
/// `crates/vike-tradehub/src/server.rs`'s `SettingsShowSource` for the scope argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSettingsShow {
    /// The settings directory the node resolved at boot and answered from, rendered — the header
    /// the CLI prints, and where the WRITE half will edit. `None` = the node runs on compiled-in
    /// defaults (no project above its working directory).
    pub settings_dir: Option<String>,
    /// One row per typed settings key, sorted by key (the builder's own order).
    pub rows: Vec<WireSettingsRow>,
}

/// One order intent to submit — a standalone mirror of the display/serde-relevant fields of
/// `vike_model::OrderRequest` (the single order-intent schema authority). An empty `client_order_id`
/// asks the runtime to mint one; a non-empty one is respected.
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
    /// **WHICH ACCOUNT of `venue`.** `None` means the sender named no account at all.
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing, not tidiness.** A client that names no account
    /// must put a frame on the wire that is BYTE-IDENTICAL to the one it sends today, so the
    /// existing round-trip fixtures do not move and an old node sees exactly what it always saw.
    ///
    /// The three wire states are read with `vike_model::account_keys::parse_wire_account`, which
    /// admits `DEFAULT` where `policy.toml` refuses it — absent means *named nothing*, `"DEFAULT"`
    /// means *named the unlabelled account*, and a label means that account. The distinction
    /// matters because ABSENCE cannot be trusted across a version boundary: an old node DROPS a
    /// field it does not know, and what arrives is indistinguishable from naming nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// The command vocabulary a [`Scope::Write`](crate::proto::Scope) client may issue —
/// a standalone mirror of the core-facing verbs in `vike_exec::{Command, OrderIntent}`. Deliberately
/// the SESSION-relevant subset a thin GUI drives (submit / cancel / mass-cancel / flatten / trading
/// state, plus the strategy-level `UpdateParams` since split-plane B4), NOT the full internal
/// reconcile/journal command surface (`ApplySnapshot`,
/// `ReconcileReports`, `ConfirmRecon`, …), which is core-internal plumbing and never a remote
/// client's business. A server binding this proto lowers each variant into the core's real
/// `Command`/`OrderIntent` at its edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireCommand {
    /// Submit one order (mirrors `OrderIntent::Submit`).
    Submit(WireOrderRequest),
    /// Cancel one order by client-order-id (mirrors `OrderIntent::Cancel`).
    Cancel(String),
    /// Modify one resting order's qty and/or price (mirrors `OrderIntent::Modify`).
    Modify { client_order_id: String, new_qty: Option<f64>, new_price: Option<f64> },
    /// Cancel every live order, optionally scoped to a venue/symbol (mirrors
    /// `OrderIntent::MassCancel`). `None`/`None` = all engines + all books.
    ///
    /// `account` scopes the cancel to ONE account of `venue`, on a node that advertises
    /// `crate::proto::FEATURE_ACCOUNT_SCOPED_REDUCE` — the node carries it to the core, cancels that
    /// account's book and no other, and refuses before the Ack an account it does not hold or one
    /// named with no `venue`. The field is gated by that string, so against a node that does NOT
    /// carry it (one built before the reducing verbs learned their account, which drops the field
    /// and cancels across every account of the venue) the frame is refused client-side before it is
    /// sent. See [`WireOrderRequest::account`] for the three wire states and why absence cannot be
    /// read as a default — an absent account still means every account of the venue, deliberately.
    MassCancel {
        venue: Option<String>,
        symbol: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// Close the `(venue, symbol)` net position with a reduce-only market order (mirrors
    /// `OrderIntent::Flatten`). No-op when flat. An absent `account` closes it on every account of
    /// `venue`; a named one narrows it to that account, under the same capability and refusals as
    /// [`WireCommand::MassCancel`]'s `account`.
    Flatten {
        venue: String,
        symbol: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// PANIC BUTTON — cancel every live order, then flatten every open position (mirrors
    /// `OrderIntent::MarketExit`). `venue: None` = every engine.
    ///
    /// ⚠ **An absent `account` FANS OUT here; it does not refuse** — the RISK-DIRECTION LAW of
    /// `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §4.5:
    /// *"A risk-REDUCING venue verb that names no account fans out to EVERY account of that venue.
    /// A risk-INCREASING one refuses."* So this verb, `MassCancel` and `SetTradingState` reach all
    /// fifty binance engines at `N = 50`, which is the only reading of *close everything on
    /// binance* that is not a trap — while a `Submit` naming no account refuses. The panic button
    /// gets WIDER at fifty accounts rather than narrower, and a fan-out can never reach an account
    /// the sender did not mean, because the sender meant all of them.
    ///
    /// **Naming an account narrows the exit to that account's book** — both legs, the cancel and
    /// the flatten, and the core-held protection (held bracket exits, armed conditionals) with
    /// them — on a node that advertises `crate::proto::FEATURE_ACCOUNT_SCOPED_REDUCE`. Such a node
    /// refuses before the Ack an account it does not hold, and an account named with NO `venue`:
    /// a label names one book OF a venue, and the arm a venue-less exit reaches is the global one.
    /// Against a node that does NOT carry the string — one that drops the field and fans the exit
    /// out over every account of the venue, so a frame naming `ALT` would flatten the unlabelled
    /// book too — `crate::remote_control::required_feature` refuses the frame client-side and
    /// nothing goes on the wire.
    MarketExit {
        venue: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// Set the account trading state / kill switch (mirrors `Command::SetTradingState`).
    SetTradingState(WireTradingState),
    /// LIVE PARAMETER update for a mounted strategy (mirrors `Command::UpdateParams` /
    /// `vike_exec::ParamsUpdate` — split-plane B4, the STRATEGY-level write verb).
    /// `(venue, symbol, interval)` names the target mount EXACTLY — the same key the core's bar
    /// path resolves a mount by; a key naming no mount is a silent no-op in the core (the same
    /// contract as an unknown modify tag).
    ///
    /// `params` is the core's own `vike_model::StrategyParams` in its OWN serde JSON form (the
    /// externally-tagged union the journaled command lane already persists, e.g.
    /// `{"SpreadMaker": { … }}`) — carried as [`serde_json::Value`], NOT re-declared here.
    /// Deliberate, and the ONE departure from this module's mirror-the-fields idiom: the union has
    /// three variants of ~17 tunables each and grows with every strategy, so a hand mirror would
    /// drift within a release, and its JSON form is already a persisted schema (old journals must
    /// replay) — the wire DELEGATES to that schema rather than duplicating it. The daemon
    /// deserializes at its edge (`lower_command`) and REFUSES an undecodable payload with a
    /// `Response::Error`, never silently dropping it. Requires the node to advertise
    /// [`crate::proto::FEATURE_STRATEGY_VERBS`]; the client refuses client-side otherwise.
    UpdateParams {
        /// Target mount venue (exact match).
        venue: String,
        /// Target mount symbol (exact match).
        symbol: String,
        /// Target mount bar interval (exact match, e.g. `"1m"`).
        interval: String,
        /// The typed `vike_model::StrategyParams` payload, in the core's own serde JSON form.
        params: serde_json::Value,
    },
    /// RUNTIME strategy MOUNT (mirrors `Command::MountStrategy` / `vike_exec::MountSpec` —
    /// split-plane B5): add one strategy mount to the running node's core without a restart. The
    /// strategy SOURCE is the profile `[strategy]` vocabulary verbatim — a registry `name` XOR a
    /// `rhai` script path (docs/decisions/0024-rhai-strategies-live.md) — plus the free-form
    /// `params` table as JSON (the `UpdateParams` delegate-don't-mirror idiom: each strategy reads
    /// its own knobs, so the wire never re-declares them). The daemon VALIDATES at its edge with
    /// the same refusals a profile load applies (`Response::Error`, never a silent drop); a
    /// refusal the core itself raises later (duplicate live id, unknown venue) surfaces in the
    /// node's recent-events instead — the `UpdateParams` unknown-target contract. Requires the
    /// node to advertise [`crate::proto::FEATURE_MOUNT_VERBS`]; the client refuses client-side
    /// otherwise.
    MountStrategy {
        /// Target venue — must name an engine the node's core already runs.
        venue: String,
        /// **WHICH ACCOUNT of `venue` the mount trades and reads** — the three wire states and why
        /// absence cannot be read as a default are [`WireOrderRequest::account`]'s doc, stated once
        /// there. Requires the node to advertise [`crate::proto::FEATURE_MOUNT_ACCOUNT`]; a client
        /// that would SET it against a node without that string refuses locally and sends nothing.
        ///
        /// ⚠ **The stakes are a rung above the order field's, and that is the whole reason this
        /// verb has its own capability.** A misrouted order is one order on the wrong book; a
        /// misrouted MOUNT is every order that strategy will ever place, for as long as it runs —
        /// and it sizes against that book's position too, so the read half is wrong in the same
        /// breath as the write half.
        ///
        /// ⚠ **A label naming an account the node does not hold is a `Response::Error` BEFORE the
        /// Ack**, not one of the recent-events refusals the variant doc above describes — the node
        /// answers in `vike-tradehub`'s `account_refusal`, the same sentence a misaddressed submit
        /// gets. That is a CORRECTION to this verb's original contract, which routed every
        /// core-side mount refusal out of band: the account is the one the client can be told about
        /// while it is still listening, and on this verb an un-answered `Ack` would have been a
        /// client printing success over a strategy that never mounted.
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
    /// RUNTIME strategy UNMOUNT (mirrors `Command::UnmountStrategy` — split-plane B5): remove the
    /// mount whose MOUNT ID matches (the explicit controller id, or the derived
    /// `{venue}__{symbol}__{interval}` id). The node's core CANCELS that mount's attributed live
    /// orders before removal and saves its durable state — the documented safe default; positions
    /// are NOT flattened (`Flatten` is the operator verb for that). An unknown id surfaces in the
    /// node's recent-events (the `UpdateParams` unknown-target contract). Requires
    /// [`crate::proto::FEATURE_MOUNT_VERBS`].
    UnmountStrategy {
        /// The mount id to remove.
        controller_id: String,
    },
    /// SETTINGS write (split-plane REQ-7, write half): set ONE ROW of the node's settings
    /// database — `<settings>/db/vike.db` — identified by its KEY (`docs/decisions/0086`). The
    /// key's first segment names the section (`policy` / `config` / `preferences` / `flags`); the
    /// node validates the row with its own `vike-config` loader BEFORE it commits, inside one
    /// transaction, so a write can never leave a store the next boot refuses. Deliberately
    /// STRINGLY, like the read half's `WireSettingsRow`: this light client crate takes no
    /// `vike-config` dependency, and the daemon is the typing/validation authority.
    ///
    /// RESTART vs HOT-APPLY (v2): an accepted write answers `Response::SettingsWritten`, whose
    /// `restart_required` is decided SERVER-side by the node's per-key classification — a
    /// hot-safe key the node actually applied live answers `false`; everything else (including
    /// every `policy.*` key, which is never hot) answers `true`: the running node keeps its
    /// boot-time value and the row is what the NEXT boot loads (the same window the read half's
    /// re-read freshness already shows). The daemon's audit record carries the old and new values.
    ///
    /// ⚠ **`file` and `confirm` are FILE-ERA fields the node ignores**, and are on their way out in
    /// two steps. Step 1 (this crate today): the node reads neither — it derives the section from
    /// `key`, and the typed confirm was deleted for every key by 0086 point 7 — and both decode as
    /// absent (`#[serde(default)]`), so a client may omit them. Step 2, after a release carrying
    /// that tolerance is deployed: delete both. Until then every client KEEPS SENDING `file`,
    /// derived from the key's first segment, and `confirm: None`, because the released v0.1.35
    /// daemon still REQUIRES `file` on decode.
    ///
    /// Requires the node to advertise [`crate::proto::FEATURE_SETTINGS_WRITE`]; the client
    /// refuses client-side otherwise (`crate::remote_control::set_setting` enforces it).
    SetSetting {
        /// IGNORED by the node — see the variant doc. A client sends the key's own section word
        /// (`"policy"` for `policy.max_notional_per_order`) until step 2 removes the field; absent
        /// decodes as empty.
        #[serde(default)]
        file: String,
        /// The FULL dotted key exactly as the read half's `WireSettingsRow::key` renders it
        /// (`"config.tradehub_addr"`, `"policy.max_notional_per_order"`) — the row's whole
        /// identity: its first segment is the section, the rest names the setting within it.
        key: String,
        /// The new value, as TEXT: parsed as a TOML value (`250`, `true`, `["a"]`) when it is
        /// one, else stored as a string — and then the row is validated with the loader, so a
        /// type the key cannot take is refused with the loader's own message.
        value: String,
        /// IGNORED by the node since `docs/decisions/0086` point 7 (no retype confirm, for any
        /// key); every client sends `None`. Absent decodes as `None`.
        #[serde(default)]
        confirm: Option<String>,
    },
}

impl WireCommand {
    /// **The VENUE this command ADDRESSES**, or `None` when it addresses none.
    ///
    /// The node routes an order by this string and nothing else: the daemon's `lower_command`
    /// copies it onto `vike_model::OrderRequest::venue`, and `vike_core`'s `CoreThread::route_of`
    /// resolves it to an engine index. So this is the ONE addressing key on this wire — there is
    /// deliberately no second one, and a client that wants a different book says so here.
    ///
    /// ⚠ **`Some` and `None` are different claims, not a nullable field.** `Some(v)` is "act on
    /// venue `v`", which a node that runs no engine for `v` must REFUSE rather than lower (see
    /// `vike-tradehub`'s `venue_refusal`, and [`crate::proto::FEATURE_VENUE_ROUTING`] for how a
    /// client learns whether the node it is talking to does). `None` is "this command names no
    /// venue", and it means one of three unrelated things, none of which is an omission:
    ///
    /// * **an ORDER-scoped verb** ([`WireCommand::Cancel`], [`WireCommand::Modify`]) — the target
    ///   is a client-order-id, and the core resolves the engine from its own `coid_venue` map. The
    ///   venue is not merely absent, it would be redundant AND overridable.
    /// * **an ACCOUNT-wide verb** ([`WireCommand::SetTradingState`], and the `venue: None` arms of
    ///   [`WireCommand::MassCancel`] / [`WireCommand::MarketExit`]) — naming the WHOLE set IS the
    ///   address. The panic button must never require an argument.
    /// * **a verb that is not about a book at all** ([`WireCommand::UnmountStrategy`], addressed by
    ///   mount id; [`WireCommand::SetSetting`], which never enters the core).
    ///
    /// NO WILDCARD ARM, deliberately: a new variant is a compile error here until somebody decides
    /// which of those classes it belongs to, which is exactly the decision that must not be made by
    /// default — a wildcard would classify a new order verb as address-less and silently hand it
    /// back the routing this method exists to take away.
    pub fn addressed_venue(&self) -> Option<&str> {
        match self {
            WireCommand::Submit(req) => Some(req.venue.as_str()),
            WireCommand::Flatten { venue, .. } => Some(venue.as_str()),
            WireCommand::UpdateParams { venue, .. } => Some(venue.as_str()),
            WireCommand::MountStrategy { venue, .. } => Some(venue.as_str()),
            // The two SCOPED risk-reducing verbs: `Some(v)` addresses that exchange (every account
            // of it, per `CoreThread::exit_scope_engines`), `None` addresses every engine and is
            // never refused — see this method's doc.
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

// -------------------------------------------------------------------------------------------
// The ACCOUNT ADMIN plane (decision 0065)
// -------------------------------------------------------------------------------------------

/// **The account-administration verbs** — the settings database's `account` table, plus the one
/// verb on this wire that carries a credential VALUE.
///
/// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` is the design, and the
/// three things to know before reading a line of this type are its three parts:
///
/// 1. **STRUCTURAL.** The server holds this capability as an `Option` its BINARY constructs. `None`
///    means the process contains no path from a frame to the store and advertises no
///    [`crate::proto::FEATURE_ACCOUNT_VERBS`], so the verb is refused because there is nothing to
///    refuse WITH — not because somebody remembered a check.
/// 2. **AUTHORIZATION.** Every verb here requires `Scope::Account`, a THIRD key. The scope byte is
///    inside the signed preimage, so a Control mac cannot satisfy an Admin challenge: the key every
///    desktop carries to place orders is not the key that writes key material.
/// 3. **CONFIDENTIALITY — DECLARED, because the process cannot know it.** This wire is PLAINTEXT
///    and authenticates the CONNECTION rather than each frame
///    (`crates/vike-tradehub/src/server.rs`'s module doc), so confidentiality comes entirely from
///    REACHABILITY — a loopback bind behind an SSH tunnel, or a barrier outside the process the
///    operator declares. 0065 §3b is why "refuse unless the listener is on loopback" is the WRONG
///    predicate in three independent ways, and §3c is the three-valued declaration that replaces it.
///
/// # ⚠ NOT a [`WireCommand`], and the separation is structural rather than tidy
///
/// [`WireCommand`] is *a standalone mirror of the core-facing verbs in `vike_exec::{Command,
/// OrderIntent}`* — its own doc — and every one of its variants is lowered into the core, rate- and
/// notional-vetted as an order, and routed by [`WireCommand::addressed_venue`]. None of that
/// applies here: an account verb enters no core, names no book and sizes nothing.
///
/// The load-bearing half is `Debug`. [`WireCommand`] DERIVES it, and 0065 §4.1 says a variant
/// carrying a credential value may not ride that derive. Keeping this plane out of that enum means
/// the derive stays honest for all twelve of its variants rather than being replaced by a hand
/// impl a thirteenth could silently rejoin — and [`AccountVerb`] carries its own redacting impl
/// below, in the shape of `vike_bridge_core::credentials::Debug for Credentials` (minus that one's
/// four-character key tail, which this verb has no reason to disclose).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountRequest {
    /// Which act.
    pub verb: AccountVerb,
    /// **The typed confirm**, for the two verbs that require one ([`AccountVerb::Remove`] and
    /// [`AccountVerb::SetCredential`]).
    ///
    /// ⚠ **The client may NEVER pre-fill this from a field it already holds**, and that is the
    /// whole of the contract: *"the friction IS the protection"*. It was modelled on
    /// `WireCommand::SetSetting`'s policy retype, which `docs/decisions/0086` point 7 has since
    /// deleted for every SETTINGS key (`crates/vike-cli/src/cmd/trade.rs` keeps the tombstone of
    /// the prompt that collected it, `typed_key_confirm`); this account-plane confirm is decision
    /// 0065's and is untouched by that ruling. The server enforces it
    /// (`crates/vike-tradehub/src/server.rs`'s `AccountAdminSource`), so no client can quietly skip
    /// the ceremony; missing and mismatched confirms get distinct messages there, each naming the
    /// expected spelling.
    ///
    /// Ignored entirely by the verbs that require none.
    #[serde(default)]
    pub confirm: Option<String>,
}

/// The five lifecycle acts, plus the credential write beside them.
///
/// ⚠ **`Debug` is HAND-WRITTEN on [`AccountRequest`] and redacts [`AccountVerb::SetCredential`]'s
/// value** — see that impl. Deriving it here would put a credential into every `{:?}` this type
/// ever reaches, and the reachable ones are not hypothetical: a panic message in a test prints to a
/// CI log.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum AccountVerb {
    /// LIST the account rows — ids, venue, tier, label, book, active, and the credential key
    /// NAMES each row owns.
    ///
    /// ⚠ **It is `Scope::Account` like every other verb here, and that is the one place 0065 NARROWS
    /// rather than widens.** A listing enumerates which venues hold live credentials, which is
    /// precisely the disclosure `SettingsShowSource`'s own scope argument says is deliberately NOT
    /// served to an Observe peer. **No value ever crosses**: the reader behind it
    /// (`vike_secrets::read_account_keys`) selects `name, field, account_id` and has no `value`
    /// column in its statement, so this is structural rather than a rule.
    List,
    /// ADD a row. ⚠ It ARMS NOTHING — `policy.venues.<venue>` is read above the credential store by
    /// the mount, so the venue stays PAPER until a policy edit.
    Add {
        /// A `vike_model::VENUES` id, validated at the server.
        venue: String,
        /// One of `vike_secrets::ACCOUNT_TIERS`. ⚠ `paper` IS a legal member since the
        /// 2026-09-23 `sim` -> `paper` rename: it is the account a `{VENUE}_SIM_*` credential
        /// mints, not the `policy.toml` CEILING of the same name. A venue with no credential
        /// at all still has no row here regardless of the ceiling's word.
        tier: String,
        /// The operator's name for the ROLE, or `None` for the unlabelled account.
        label: Option<String>,
    },
    /// CHANGE one row's label, and nothing else — not its id, not its book, not its key prefixes.
    Rename {
        /// The row, by `account.id`.
        id: i64,
        /// The new label, or `None` to clear it.
        label: Option<String>,
    },
    /// DEACTIVATE or re-activate a row — the reversible act, and the one a UI leads with.
    SetActive {
        /// The row, by `account.id`.
        id: i64,
        /// `false` = the operator no longer uses this account.
        active: bool,
    },
    /// DELETE a row. Refused while it still owns credentials, naming them by KEY NAME; requires
    /// [`AccountRequest::confirm`] to equal the id.
    Remove {
        /// The row, by `account.id`.
        id: i64,
    },
    /// **The verb that carries a credential VALUE** — one named key, upserted into the store that
    /// answers on the node's box.
    ///
    /// Requires [`AccountRequest::confirm`] to equal `key` exactly — the shape a `policy.toml`
    /// settings write required until `docs/decisions/0086` point 7 deleted that retype for every
    /// SETTINGS key; this credential verb keeps it. The server validates `key` against
    /// `vike_model::credential_keys` and refuses a multi-line value; the write itself is a CALL
    /// SITE of `vike_secrets::save_credentials_to_store` — the one upsert — never a second writer.
    ///
    /// ⚠ **Nothing ever reads it back out.** There is no `GetCredential` on this wire and there
    /// must not be: 0065's *any verb on this surface returning a credential VALUE* is a named
    /// reopener, and the listing verb's value-free statement is why the absence is structural.
    SetCredential {
        /// The credential key NAME, validated against the grid at the server.
        key: String,
        /// The value. **One line.** ⚠ It rides no `Debug`, reaches no log, no error message and no
        /// reply — see [`AccountRequest`]'s `Debug` impl, which is what enforces the first of
        /// those.
        value: String,
    },
    /// **WRITE one row's `venue_account_id` — the BOOK, as the venue names it.** The wire twin of
    /// `vike-cli secrets set-book`, and the verb that makes an account's identity settable from a
    /// desktop rather than only from a shell on the node's box.
    ///
    /// ⚠ **This decides which BROKER an order is attributed to.** Dukascopy's two demo accounts are
    /// two legal entities; on the CEX venues it is the value that lets the mount see two labels
    /// pointing at ONE venue account and say so. That is why the overwrite is gated below rather
    /// than being an ordinary field write.
    ///
    /// It carries **no credential and no secret** — a venue account id is the number the venue
    /// prints on its own page and echoes on its own wire (`vike_secrets::Account::venue_account_id`
    /// states the same), which is why it rides the ordinary `Debug` while
    /// [`AccountVerb::SetCredential`]'s value may not.
    SetBook {
        /// The row, by `account.id`.
        id: i64,
        /// The book, or `None` to put the column back to *not yet known*. The server normalises and
        /// validates it through `vike_secrets::normalized_venue_account_id` — the SAME function the
        /// CLI calls, so the two surfaces cannot accept different values.
        venue_account_id: Option<String>,
        /// **Permission to REPOINT a row that already names a different book**, mirroring the CLI's
        /// `--replace`. Without it `vike_secrets::set_venue_account_id` refuses that case
        /// (`DbErrorKind::BookAlreadyKnown`), and the refusal is the point: an operator-stated book
        /// the venue disagrees with is a FINDING, not a stale value to correct silently.
        ///
        /// ⚠ Writing onto an EMPTY column needs none of this, and that asymmetry is deliberate —
        /// the ordinary act (telling the store what it did not know) stays a plain write, and only
        /// the act that moves order attribution carries ceremony. [`AccountVerb::required_confirm`]
        /// keys on exactly this flag for the same reason.
        replace: bool,
    },
}

impl AccountVerb {
    /// A short, stable word for the act — for a log line and a refusal. Total by construction, so a
    /// new variant is a compile error rather than a record with a wrong verb.
    ///
    /// ⚠ **This is what a server may log, and `{self:?}` is not.** The `Debug` impl below redacts,
    /// but a redacting impl is a promise; this method cannot carry a value at all.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            AccountVerb::List => "list",
            AccountVerb::Add { .. } => "add",
            AccountVerb::Rename { .. } => "rename",
            AccountVerb::SetActive { active: true, .. } => "activate",
            AccountVerb::SetActive { active: false, .. } => "deactivate",
            AccountVerb::Remove { .. } => "remove",
            AccountVerb::SetCredential { .. } => "set-credential",
            // ⚠ Two words, not one. A CLEAR and a WRITE are different acts in a log line and in a
            // refusal, and the CLI spells them apart too (`--clear` is its own flag there).
            AccountVerb::SetBook { venue_account_id: Some(_), .. } => "set-book",
            AccountVerb::SetBook { venue_account_id: None, .. } => "clear-book",
        }
    }

    /// **What [`AccountRequest::confirm`] must equal**, or `None` for a verb that needs no ceremony.
    ///
    /// ONE derivation, consulted by the client that asks the operator to type it and by the server
    /// that refuses anything else — so the two surfaces cannot answer differently about what the
    /// ceremony is.
    ///
    /// ⚠ A client may call this to know WHETHER to prompt. It may NOT call it to FILL the box: the
    /// contract is that the operator types it, and a pre-filled confirm is a click.
    #[must_use]
    pub fn required_confirm(&self) -> Option<String> {
        match self {
            AccountVerb::Remove { id } => Some(id.to_string()),
            AccountVerb::SetCredential { key, .. } => Some(key.clone()),
            // ⚠ **Only when REPOINTING.** Writing a book onto a column that holds none is the
            // ordinary act — telling the store what it did not know — and gating it would put
            // ceremony on the path an operator walks every time they configure a box. Moving a row
            // that ALREADY names a different book is the act that changes which broker an order is
            // attributed to, and that one is typed. Same split the CLI draws with `--replace`.
            AccountVerb::SetBook { id, replace: true, .. } => Some(id.to_string()),
            AccountVerb::SetBook { replace: false, .. }
            | AccountVerb::List
            | AccountVerb::Add { .. }
            | AccountVerb::Rename { .. }
            | AccountVerb::SetActive { .. } => None,
        }
    }
}

/// ⚠ **HAND-WRITTEN, and the one field it must never print is [`AccountVerb::SetCredential`]'s
/// `value`.**
///
/// `WireCommand` derives `Debug` and 0065 §4.1 states the rule this impl exists for: *"a variant
/// carrying a value may not ride that derive … The variant prints the key NAME and the word
/// `set`."* The shape is `vike_bridge_core::credentials::Debug for Credentials`, minus that one's
/// four trailing characters of the key — a disclosure this verb has no reason to make.
///
/// It is on the REQUEST rather than on [`AccountVerb`] alone so that no derive anywhere can reach
/// the value through a wrapper: `AccountRequest` has no `Debug` derive either, and the only way to
/// format the pair is through this.
impl std::fmt::Debug for AccountRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The confirm is a KEY NAME or an id — never a secret — but it is rendered as a presence
        // mark anyway: it is an operator-typed field on a frame that also carries a credential, and
        // a `Debug` that printed one operator-typed field and redacted the other is a `Debug` a
        // reader has to check.
        let confirm = if self.confirm.is_some() { "typed" } else { "absent" };
        match &self.verb {
            AccountVerb::SetCredential { key, .. } => {
                write!(f, "AccountRequest(set-credential key={key} value=<set> confirm={confirm})")
            }
            AccountVerb::Add { venue, tier, label } => write!(
                f,
                "AccountRequest(add venue={venue} tier={tier} label={} confirm={confirm})",
                label.as_deref().unwrap_or("(none)")
            ),
            AccountVerb::Rename { id, label } => write!(
                f,
                "AccountRequest(rename id={id} label={} confirm={confirm})",
                label.as_deref().unwrap_or("(none)")
            ),
            AccountVerb::SetActive { id, active } => {
                write!(f, "AccountRequest(set-active id={id} active={active} confirm={confirm})")
            }
            AccountVerb::Remove { id } => {
                write!(f, "AccountRequest(remove id={id} confirm={confirm})")
            }
            // ⚠ The book IS printed, unlike `SetCredential`'s value, and that is a claim rather
            // than an oversight: a venue account id is the number the venue prints on its own page
            // and echoes on its own wire. `vike_secrets::Account::venue_account_id` and
            // `vike_mount::book_identity`'s module doc both hold the same line — a row may name an
            // address, a login or an account id, never anything derived from a credential.
            AccountVerb::SetBook { id, venue_account_id, replace } => write!(
                f,
                "AccountRequest(set-book id={id} book={} replace={replace} confirm={confirm})",
                venue_account_id.as_deref().unwrap_or("(clear)")
            ),
            AccountVerb::List => write!(f, "AccountRequest(list confirm={confirm})"),
        }
    }
}

/// One `account` row as [`AccountVerb::List`] renders it — a standalone mirror of
/// `vike_secrets::Account` plus the row's credential key NAMES, for this light client crate's
/// mirror-the-fields idiom (it takes no `vike-secrets` dependency).
///
/// ⚠ **`keys` are NAMES and there is no value field**, which is the same structural property the
/// reader has: `vike_secrets::read_account_keys`' statement selects `name, field, account_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAccountRow {
    /// `account.id` — the identity, and stable for the life of ONE database file. ⚠ Never write it
    /// down: a re-migration renumbers the rows.
    pub id: i64,
    /// The row's venue id.
    pub venue: String,
    /// The row's tier — `paper` / `demo` / `live` (`vike_secrets::ACCOUNT_TIERS`). ⚠ CARRIED RAW,
    /// with no serde rename: a node older than the 2026-09-23 `sim` -> `paper` rename answers `sim`
    /// here and a CLI older than it SENDS `sim` on `AccountVerb::Add`. Both are REFUSED BY NAME
    /// rather than silently — the server validates against `ACCOUNT_TIERS` and prints the legal
    /// set — so the skew is a message an operator can act on, never a wrong row.
    pub tier: String,
    /// The operator's name for the ROLE. `None` is the ordinary answer, and a reader may not
    /// synthesise one.
    pub label: Option<String>,
    /// The BOOK as the venue names it. `None` means *not yet known*, never *this account has no
    /// book*.
    pub venue_account_id: Option<String>,
    /// `false` = deactivated. Every consumer of the arming reader already treats such a row exactly
    /// as it would treat a deleted one.
    pub active: bool,
    /// When a venue's own handshake last confirmed this row, RFC 3339 — `None` = never verified.
    pub last_verified_at: Option<String>,
    /// The live credential key NAMES this row owns. Names, never values.
    pub keys: Vec<String>,
}

/// The [`crate::proto::Response::AccountList`] payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAccountList {
    /// The store the node read, as a display path — the same `store:` line
    /// `vike-cli secrets account` prints first, and for its reason: a listing from the wrong box
    /// reads exactly like a listing from the right one.
    pub store: String,
    /// Every row, `id`-ordered. INACTIVE rows included — filtering is the reader's decision, and a
    /// listing that hid them would make a deactivated account look removed.
    pub rows: Vec<WireAccountRow>,
}

/// The [`crate::proto::Response::AccountWritten`] payload — what an accepted write DID.
///
/// ⚠ It carries no credential value on any verb, [`AccountVerb::SetCredential`] included: what a
/// caller needs back from that one is the key NAME and the fact that it landed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAccountWritten {
    /// [`AccountVerb::word`] for the act performed.
    pub verb: String,
    /// The row the act names — `None` for [`AccountVerb::SetCredential`], which names a key rather
    /// than a row, and for a [`AccountVerb::Remove`] that left none.
    pub row: Option<WireAccountRow>,
    /// `false` when the store already held exactly this state and nothing was written — a rename to
    /// the label already carried, a deactivate of an already-inactive row. Idempotent rather than
    /// an error.
    pub changed: bool,
    /// One sentence the node wants the operator to read — the *this arms NOTHING* line on an `add`,
    /// the *a running daemon does not notice until it restarts* line on a deactivate. **Never a
    /// value**: it is composed by the server from ids, venues, tiers and key NAMES.
    pub note: Option<String>,
}
#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;

#[path = "the_account_field_is_invisible_until_it_is_used.rs"]
#[cfg(test)]
mod the_account_field_is_invisible_until_it_is_used;
