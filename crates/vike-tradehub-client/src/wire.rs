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
    /// route lookup can be wrong about; `vike_app_core::backend_identity` is where it is rendered.
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
    /// The settings FILE this key belongs to (`"policy.toml"`, `"config.toml"`, …) — the CLI's
    /// grouping, and the file the WRITE half (a follow-up) will edit.
    pub section: String,
    /// The dotted key (`"config.tradehub_addr"`); its first segment is the settings section.
    pub key: String,
    /// The EFFECTIVE value, rendered; `""` = unset. Already redacted for a secret-shaped key.
    pub value: String,
    /// The layer that set it, as the CLI's one ORIGIN cell: `"default"`, `"policy.toml"`,
    /// `"env:VIKE_RECONCILE"`.
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
    /// The three wire states are read with [`vike_model::account_keys::parse_wire_account`], which
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
    /// `account` scopes it further, to ONE account of `venue` — see [`WireOrderRequest::account`]
    /// for the three wire states and why absence cannot be read as a default.
    MassCancel {
        venue: Option<String>,
        symbol: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    /// Close the `(venue, symbol)` net position with a reduce-only market order (mirrors
    /// `OrderIntent::Flatten`). No-op when flat.
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
    /// Naming an account narrows this verb back to that account, which is the whole of what the
    /// field adds here.
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
    /// SETTINGS write (split-plane REQ-7, write half): set ONE key in ONE of the node's four
    /// settings files (`policy.toml` / `config.toml` / `preferences.toml` / `flags.toml`),
    /// comment-preserving and validated server-side with the node's own `vike-config` loader
    /// BEFORE anything lands on disk — the write can never produce a file the next boot refuses.
    /// Deliberately STRINGLY, like the read half's `WireSettingsRow`: this light client crate
    /// takes no `vike-config` dependency, and the daemon is the typing/validation authority.
    ///
    /// RESTART vs HOT-APPLY (v2): an accepted write answers `Response::SettingsWritten`, whose
    /// `restart_required` is decided SERVER-side by the node's per-key classification — a
    /// hot-safe key the node actually applied live answers `false`; everything else (including
    /// every `policy.toml` key, which is never hot) answers `true`: the running node keeps its
    /// boot-time value and the edit is what the NEXT boot loads (the same window the read
    /// half's re-read freshness already shows).
    ///
    /// ⚠ **THE TYPED-CONFIRM CONTRACT for `policy.toml`.** A `SetSetting` whose `file` is the
    /// policy file is REFUSED unless `confirm` carries the EXACT dotted `key` being changed —
    /// policy holds the risk ceilings, so a client must make the operator retype the key name
    /// (never pre-fill it). Non-policy files need no confirm and ignore the field. The daemon's
    /// audit record for a policy write carries the old and new values.
    ///
    /// Requires the node to advertise [`crate::proto::FEATURE_SETTINGS_WRITE`]; the client
    /// refuses client-side otherwise (`crate::remote_control::set_setting` enforces it).
    SetSetting {
        /// Which settings file, by name: `"policy.toml"` / `"config.toml"` /
        /// `"preferences.toml"` / `"flags.toml"` (the bare stem without `.toml` is accepted too).
        file: String,
        /// The FULL dotted key exactly as the read half's `WireSettingsRow::key` renders it
        /// (`"config.tradehub_addr"`, `"policy.max_notional_per_order"`) — its first segment
        /// must name the same file `file` does; the rest is the path inside that file's TOML.
        key: String,
        /// The new value, as TEXT: parsed as a TOML value (`250`, `true`, `["a"]`) when it is
        /// one, else written as a string — and then the whole would-be file is validated with
        /// the loader, so a type the key cannot take is refused with the loader's own message.
        value: String,
        /// The typed confirm: MUST equal `key` exactly for a `policy.toml` write; ignored for
        /// the other three files.
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
    /// whole of the contract — `WireCommand::SetSetting`'s policy rule verbatim
    /// (`crates/vike-cli/src/cmd/trade.rs`'s `typed_key_confirm`: *"the friction IS the
    /// protection"*). The server enforces the same rule
    /// (`crates/vike-tradehub/src/server.rs`'s `AccountAdminSource`), so no client can quietly skip the
    /// ceremony; missing and mismatched confirms get distinct messages there, each naming the
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
        /// One of `vike_secrets::ACCOUNT_TIERS`. ⚠ Never `paper`: a paper venue loads no
        /// credential and so has no account row.
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
    /// Requires [`AccountRequest::confirm`] to equal `key` exactly, the same shape a `policy.toml`
    /// write requires. The server validates `key` against `vike_model::credential_keys` and refuses
    /// a multi-line value; the write itself is a CALL SITE of
    /// `vike_secrets::save_credentials_to_store` — the one upsert — never a second writer.
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
    /// The row's tier — `sim` / `demo` / `live`.
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
#[cfg(test)]
mod tests {
    use super::*;

    fn full_snapshot() -> WireSnapshot {
        WireSnapshot {
            seq: 42,
            accounts_epoch: 0,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trading_state: WireTradingState::Reducing,
            balance: 10_000.0,
            equity_total: 12_345.67,
            venues: vec![WireVenueBlock {
                venue: "binance".into(),
                balance: 10_000.0,
                realized_pnl: 250.5,
                fees_paid: 3.25,
                funding_paid: -1.5,
                equity: 12_345.67,
                unrealized: 2_345.67,
                missing_prices: 1,
                margin_used: 500.0,
                free_bp: 11_845.67,
                trading_state: WireTradingState::Reducing,
                account: None,
                route_key: "binance".into(),
                positions: vec![WirePositionView {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    position_side: "BOTH".into(),
                    size: 0.5,
                    avg_px: 60_000.0,
                    unrealized: 2_345.67,
                    leverage: 5.0,
                    liq_price: 48_000.0,
                }],
            }],
            orders: vec![WireOrderView {
                client_order_id: "c-1".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 0.5,
                order_type: "limit".into(),
                price: Some(59_000.0),
                trigger_price: None,
                status: "Accepted".into(),
                venue_order_id: Some("v-9".into()),
                filled_qty: 0.0,
                avg_fill_px: 0.0,
            }],
            positions: vec![WirePositionView {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: "BOTH".into(),
                size: 0.5,
                avg_px: 60_000.0,
                unrealized: 2_345.67,
                leverage: 5.0,
                liq_price: 48_000.0,
            }],
            held_exits: vec![WireHeldOrderView {
                client_order_id: "c-2".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 0.5,
                order_type: "stop".into(),
                price: None,
                trigger_price: Some(55_000.0),
                parent_order_id: Some("c-1".into()),
            }],
            recent_events: vec!["OrderAccepted c-1".into(), "OrderSubmitted c-1".into()],
            fault: None,
            bars: vec![WireBarSeries {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                closed: vec![
                    WireBar {
                        ts: 1_000,
                        o: 60_000.0,
                        h: 60_100.0,
                        l: 59_900.0,
                        c: 60_050.0,
                        v: 12.5,
                    },
                    WireBar {
                        ts: 61_000,
                        o: 60_050.0,
                        h: 60_200.0,
                        l: 60_000.0,
                        c: 60_150.0,
                        v: 8.0,
                    },
                ],
                forming: Some(WireBar {
                    ts: 121_000,
                    o: 60_150.0,
                    h: 60_180.0,
                    l: 60_120.0,
                    c: 60_170.0,
                    v: 3.2,
                }),
            }],
            identity: None,
        }
    }

    /// A fully-populated snapshot round-trips through `serde_json` byte-stably (every Some arm, a
    /// non-empty venue/order/position/held/events set).
    #[test]
    fn wire_snapshot_round_trips_fully_populated() {
        let snap = full_snapshot();
        let js = serde_json::to_string(&snap).unwrap();
        let back: WireSnapshot = serde_json::from_str(&js).unwrap();
        assert_eq!(snap, back);
    }

    /// Every `WireCommand` variant round-trips through JSON.
    #[test]
    fn wire_command_round_trips_each_variant() {
        let cmds = vec![
            WireCommand::Submit(WireOrderRequest {
                client_order_id: String::new(),
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                order_type: "market".into(),
                price: None,
                trigger_price: None,
                reduce_only: false,
                account: None,
            }),
            WireCommand::Cancel("c-1".into()),
            WireCommand::Modify {
                client_order_id: "c-1".into(),
                new_qty: Some(2.0),
                new_price: None,
            },
            WireCommand::MassCancel { venue: None, symbol: None, account: None },
            WireCommand::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
            WireCommand::MarketExit { venue: Some("binance".into()), account: None },
            WireCommand::SetTradingState(WireTradingState::Halted),
            // The B4 strategy write verb: the params payload is the CORE's own externally-tagged
            // StrategyParams JSON, carried opaquely (delegated, not mirrored — see the variant doc).
            WireCommand::UpdateParams {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                params: serde_json::json!({"SpreadMaker": {"qty": 2.0, "half_spread": 1.0}}),
            },
            // The B5 mount verbs: both source arms (registry name / rhai path) + the unmount.
            WireCommand::MountStrategy {
                venue: "binance".into(),
                account: Some("ALT".into()),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: Some("grid-a".into()),
                name: Some("grid".into()),
                rhai: None,
                params: serde_json::json!({"qty": 1.0}),
            },
            WireCommand::MountStrategy {
                venue: "binance".into(),
                account: None,
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: None,
                name: None,
                rhai: Some("strategies/breaker.rhai".into()),
                params: serde_json::json!({}),
            },
            WireCommand::UnmountStrategy { controller_id: "grid-a".into() },
            // The REQ-7 settings write: a confirm-less non-policy edit and a policy edit whose
            // typed confirm names the exact key.
            WireCommand::SetSetting {
                file: "config.toml".into(),
                key: "config.tradehub_addr".into(),
                value: "127.0.0.1:7879".into(),
                confirm: None,
            },
            WireCommand::SetSetting {
                file: "policy.toml".into(),
                key: "policy.max_notional_per_order".into(),
                value: "250".into(),
                confirm: Some("policy.max_notional_per_order".into()),
            },
        ];
        for c in cmds {
            let js = serde_json::to_string(&c).unwrap();
            let back: WireCommand = serde_json::from_str(&js).unwrap();
            assert_eq!(c, back);
        }
    }

    /// The B4 status payload round-trips fully populated — identity + effective params + a mount
    /// row — and an EMPTY mounts `Vec` (a node that mounted nothing) survives too.
    #[test]
    fn wire_strategy_status_round_trips() {
        let full = WireStrategyStatus {
            identity: WireNodeIdentity {
                name: "the build runner".into(),
                strategy: "spread_maker".into(),
                params: "qty=1 half_spread=0.5".into(),
                live: true,
                build: "vike 0.1.0 (abc1234)".into(),
                advertise_addr: "203.0.113.7:7879".into(),
            },
            effective_params: "qty=1 half_spread=0.5".into(),
            mounts: vec![WireMountRow {
                strategy: "spread_maker".into(),
                params: "qty=1 half_spread=0.5".into(),
                live: true,
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                // A REAL `vike_model::StrategyParams` JSON shape (externally tagged, one variant
                // key) rather than a toy object — this field's whole contract is that the bytes
                // round-trip unexamined, so the fixture must be the shape the core actually emits.
                typed_params: Some(serde_json::json!({
                    "SpreadMaker": { "qty": 1.0, "half_spread": 0.5, "avellaneda_stoikov": null }
                })),
                asset_class: Some("CryptoPerp".into()),
            }],
        };
        for status in [full.clone(), WireStrategyStatus { mounts: Vec::new(), ..full }] {
            let js = serde_json::to_string(&status).unwrap();
            let back: WireStrategyStatus = serde_json::from_str(&js).unwrap();
            assert_eq!(status, back);
        }
    }

    /// The additive-field contract for the params read (`a_frame_without_identity_parses_as_none`'s
    /// shape, one struct over): a `WireMountRow` from a node that predates
    /// [`crate::proto::FEATURE_STRATEGY_PARAMS`] carries none of the four new keys, and must still
    /// parse — as an EMPTY addressing key and `None` params, never an error. That is what lets them
    /// ride without a `NODE_PROTO_VERSION` bump (the version is folded into the auth MAC).
    ///
    /// ⚠ And it is exactly why the capability exists: the value this test asserts is
    /// INDISTINGUISHABLE from an honest "this mount publishes no typed params" on a NEW node. A
    /// client tells the two apart by `Welcome.features`, never by looking at these fields — the
    /// negotiated [`crate::remote_handle::strategy_params`] read is where that check lives.
    #[test]
    fn a_mount_row_without_the_params_fields_parses_as_empty() {
        let js = r#"{"strategy":"spread_maker","params":"qty=1 half_spread=0.5","live":true}"#;
        let row: WireMountRow = serde_json::from_str(js).expect("an old node's row still parses");
        assert_eq!(row.strategy, "spread_maker");
        assert!(row.live, "the pre-existing fields are untouched");
        assert_eq!((row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()), ("", "", ""));
        assert_eq!(row.typed_params, None);
    }

    /// `WireOrderRequest`'s `#[serde(default)]` fields let a minimal JSON (only the required fields)
    /// deserialize — the thin-client convenience the real `OrderRequest` also affords.
    #[test]
    fn wire_order_request_accepts_minimal_json() {
        let js = r#"{"venue":"sim","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"market"}"#;
        let req: WireOrderRequest = serde_json::from_str(js).unwrap();
        assert_eq!(req.client_order_id, "");
        assert_eq!(req.price, None);
        assert!(!req.reduce_only);
    }

    /// The identity block round-trips when present (split-plane B3): a GUI holding several
    /// backends labels them from this, and paper-vs-live must survive the wire exactly.
    #[test]
    fn identity_roundtrips_when_present() {
        let mut s = full_snapshot();
        s.identity = Some(WireNodeIdentity {
            name: "the build runner".into(),
            strategy: "spread_maker".into(),
            params: "qty=1 half_spread=0.5".into(),
            live: true,
            build: "vike 0.1.0 (abc1234)".into(),
            advertise_addr: "203.0.113.7:7879".into(),
        });
        let json = serde_json::to_string(&s).unwrap();
        let back: WireSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.identity.as_ref().unwrap().name, "the build runner");
        assert!(back.identity.as_ref().unwrap().live);
    }

    /// The additive-field contract (the `bars` precedent): a frame from an OLDER node that
    /// predates `identity` must still parse, as `None` — never an error. This is what lets the
    /// field ride WITHOUT a `NODE_PROTO_VERSION` bump (the version is folded into the auth MAC,
    /// so a bump would break the handshake against every running node).
    #[test]
    fn a_frame_without_identity_parses_as_none() {
        let mut s = full_snapshot();
        s.identity = None;
        let mut v: serde_json::Value = serde_json::to_value(&s).unwrap();
        v.as_object_mut().unwrap().remove("identity");
        let back: WireSnapshot = serde_json::from_value(v).unwrap();
        assert!(back.identity.is_none());
    }

    /// The same contract ONE STRUCT DOWN, for the field a client reads to say WHICH BOX the daemon
    /// is on: a node old enough to send an identity block but not
    /// [`WireNodeIdentity::advertise_addr`] must still parse, as EMPTY rather than as an error.
    ///
    /// ⚠ The two failure shapes this pins are different and both matter. A parse ERROR would take
    /// the whole frame down — the address field would break the snapshot, the orders and the
    /// positions of every daemon that has not been redeployed. And an empty string must not be
    /// read as a FACT about the daemon: it says "this node reported no address", which is exactly
    /// what `vike_app_core::backend_identity` renders it as.
    #[test]
    fn an_identity_without_the_advertise_field_parses_as_empty() {
        let js = r#"{"name":"the build runner","strategy":"spread_maker","params":"{}","live":true,
                     "build":"vike 0.1.0 (abc1234)"}"#;
        let id: WireNodeIdentity =
            serde_json::from_str(js).expect("an old node's identity block still parses");
        assert_eq!(id.name, "the build runner", "the pre-existing fields are untouched");
        assert!(id.live);
        assert_eq!(id.advertise_addr, "", "absent is EMPTY, never an error and never a default IP");
    }

    // -----------------------------------------------------------------------------------------
    // The ADDRESS a command carries
    // -----------------------------------------------------------------------------------------

    fn order(venue: &str) -> WireOrderRequest {
        WireOrderRequest {
            client_order_id: "c-1".into(),
            venue: venue.into(),
            symbol: "SYM".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: None,
        }
    }

    /// Every venue-carrying variant answers with ITS OWN venue field — not the first one in the
    /// struct, not a constant, not the `symbol`. The node routes on exactly this string, so a
    /// mis-wired arm here is a mis-routed order.
    #[test]
    fn every_venue_carrying_variant_answers_with_its_own_venue() {
        assert_eq!(WireCommand::Submit(order("binance")).addressed_venue(), Some("binance"));
        assert_eq!(
            WireCommand::Flatten { venue: "okx".into(), symbol: "SYM".into(), account: None }
                .addressed_venue(),
            Some("okx")
        );
        assert_eq!(
            WireCommand::MassCancel { venue: Some("bybit".into()), symbol: None, account: None }
                .addressed_venue(),
            Some("bybit")
        );
        assert_eq!(
            WireCommand::MarketExit { venue: Some("deribit".into()), account: None }
                .addressed_venue(),
            Some("deribit")
        );
        assert_eq!(
            WireCommand::UpdateParams {
                venue: "aster".into(),
                symbol: "SYM".into(),
                interval: "1m".into(),
                params: serde_json::json!({}),
            }
            .addressed_venue(),
            Some("aster")
        );
        assert_eq!(
            WireCommand::MountStrategy {
                venue: "hyperliquid".into(),
                account: None,
                symbol: "SYM".into(),
                interval: "1m".into(),
                controller_id: None,
                name: Some("buy_hold".into()),
                rhai: None,
                params: serde_json::json!({}),
            }
            .addressed_venue(),
            Some("hyperliquid")
        );
    }

    /// The three ADDRESS-LESS classes answer `None`, each for its own documented reason — an
    /// order-scoped verb, an account-wide one, and one that is not about a book at all. The
    /// UNSCOPED panic button is in here deliberately: naming the whole set IS naming the target,
    /// and a routing gate that could refuse it would be a kill switch with a prerequisite.
    #[test]
    fn the_address_less_variants_answer_none() {
        for cmd in [
            WireCommand::Cancel("c-1".into()),
            WireCommand::Modify { client_order_id: "c-1".into(), new_qty: None, new_price: None },
            WireCommand::SetTradingState(WireTradingState::Halted),
            WireCommand::MassCancel { venue: None, symbol: None, account: None },
            WireCommand::MarketExit { venue: None, account: None },
            WireCommand::UnmountStrategy { controller_id: "m-1".into() },
            WireCommand::SetSetting {
                file: "policy.toml".into(),
                key: "policy.max_notional_per_order".into(),
                value: "250".into(),
                confirm: Some("policy.max_notional_per_order".into()),
            },
        ] {
            assert_eq!(cmd.addressed_venue(), None, "{cmd:?} addresses no venue");
        }
    }

    /// The field is REQUIRED on the wire and always has been: a `Submit` body with no `venue` key
    /// does not decode. That is what makes the node-side routing gate a check on an existing field
    /// rather than a new field — there is no "old client that sends no venue" to be compatible
    /// with, and so no [`crate::proto::NODE_PROTO_VERSION`] bump anywhere in this change.
    #[test]
    fn a_submit_body_without_a_venue_key_does_not_decode() {
        let js = r#"{"Submit":{"client_order_id":"c-1","symbol":"SYM","side":1,"qty":1.0,
                     "order_type":"limit","price":1.0}}"#;
        assert!(
            serde_json::from_str::<WireCommand>(js).is_err(),
            "the venue is a plain String with no serde default — absent is an ERROR, never empty"
        );
    }
}

#[cfg(test)]
mod the_account_field_is_invisible_until_it_is_used {
    use super::*;

    /// **THE forward-compatibility property, measured rather than asserted.**
    ///
    /// A client that names no account must put a frame on the wire that is BYTE-IDENTICAL to the
    /// one it sent before the field existed. Everything else rests on this: the existing round-trip
    /// fixtures, an old node's decode, and the claim in `FEATURE_ACCOUNT_ROUTING`'s doc that
    /// absence is indistinguishable from a dropped field.
    ///
    /// The assertion is on the SERIALIZED TEXT, not on a round-trip — a round-trip would pass just
    /// as happily with `"account":null` in the JSON, which is exactly the byte an old node would
    /// choke on.
    #[test]
    fn a_command_naming_no_account_serialises_without_the_key() {
        let cmds = [
            WireCommand::MassCancel { venue: None, symbol: None, account: None },
            WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: None,
            },
            WireCommand::MarketExit { venue: Some("okx".into()), account: None },
        ];
        for c in &cmds {
            let js = serde_json::to_string(c).expect("a command serialises");
            assert!(
                !js.contains("account"),
                "a command naming no account must not mention the key at all: {js}"
            );
        }
    }

    /// ...and the order request half, which is where a Submit carries it.
    #[test]
    fn an_order_naming_no_account_serialises_without_the_key() {
        let req = WireOrderRequest {
            client_order_id: "c-1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: None,
        };
        let js = serde_json::to_string(&req).expect("an order serialises");
        assert!(!js.contains("account"), "{js}");
    }

    /// The complement, and it is what makes the test above mean something: when a client DOES name
    /// an account the key is on the wire, with the label as written.
    #[test]
    fn a_named_account_reaches_the_wire_verbatim() {
        let c = WireCommand::Flatten {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: Some("ALT".into()),
        };
        let js = serde_json::to_string(&c).expect("a command serialises");
        assert!(js.contains("\"account\":\"ALT\""), "{js}");
    }

    /// ⚠ **An OLD node's frame still decodes** — the other direction of the same property. A frame
    /// with no `account` key is what every client sends today, and `#[serde(default)]` is what
    /// keeps it decodable once the field exists.
    #[test]
    fn a_pre_field_frame_still_decodes_and_names_no_account() {
        let old = r#"{"Flatten":{"venue":"binance","symbol":"BTCUSDT"}}"#;
        let c: WireCommand = serde_json::from_str(old).expect("a pre-field frame must decode");
        match c {
            WireCommand::Flatten { account, .. } => {
                assert!(account.is_none(), "a dropped field reads as NAMED NOTHING")
            }
            other => panic!("decoded as the wrong variant: {other:?}"),
        }
    }
}
