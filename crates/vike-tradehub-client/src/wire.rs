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
}

/// The command vocabulary a [`Scope::Control`](crate::proto::Scope) client may issue —
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
    MassCancel { venue: Option<String>, symbol: Option<String> },
    /// Close the `(venue, symbol)` net position with a reduce-only market order (mirrors
    /// `OrderIntent::Flatten`). No-op when flat.
    Flatten { venue: String, symbol: String },
    /// PANIC BUTTON — cancel every live order, then flatten every open position (mirrors
    /// `OrderIntent::MarketExit`). `venue: None` = every engine.
    MarketExit { venue: Option<String> },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn full_snapshot() -> WireSnapshot {
        WireSnapshot {
            seq: 42,
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
            }),
            WireCommand::Cancel("c-1".into()),
            WireCommand::Modify {
                client_order_id: "c-1".into(),
                new_qty: Some(2.0),
                new_price: None,
            },
            WireCommand::MassCancel { venue: None, symbol: None },
            WireCommand::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into() },
            WireCommand::MarketExit { venue: Some("binance".into()) },
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
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: Some("grid-a".into()),
                name: Some("grid".into()),
                rhai: None,
                params: serde_json::json!({"qty": 1.0}),
            },
            WireCommand::MountStrategy {
                venue: "binance".into(),
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
}
