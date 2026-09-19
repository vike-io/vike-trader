//! The PORTABLE strategy registry: a name -> `Box<dyn Strategy<B> + Send>` lookup that is generic
//! over the broker, so the SAME table serves the backtest simulator and the live daemon.
//!
//! ## Why this lives here and not in `vike-backtest`
//! The registry used to be `vike_backtest::harness::registry`, returning
//! `Box<dyn Strategy<SimBroker>>` — a type only the simulator can name. That made "run the strategy
//! I backtested" impossible for `vike-tradehub` without depending on `vike-backtest`, which would
//! drag the simulator (and, under `datafusion-store`, the whole Arrow/DataFusion tree) into the
//! binary that signs real orders. That is the exact cost `vike-ops` and `vike-alerting` were split
//! out to avoid.
//!
//! The fix is the BOUND, not a new crate: [`vike_model::HftBroker`] is a sub-trait of
//! [`vike_model::Broker`], and BOTH `vike_backtest::SimBroker` (`engine::sim_broker`'s
//! `impl HftBroker for SimBroker`) and `vike_core::LiveBroker` (`runtime::broker`'s
//! `impl HftBroker for LiveBroker`) implement it. So one `strategy_by_name<B: HftBroker>` resolves
//! every strategy that is written `impl<B: Broker>` (most of them) AND the makers written
//! `impl<B: HftBroker>` ([`vike_mm::SpreadMaker`]), at either broker.
//!
//! ## What is NOT here, and why that is stated as DATA
//! Seven registry names stay in `vike-backtest` — five are `impl Strategy<SimBroker>` BY DESIGN
//! (they name simulator-only machinery) and two are portable but reach crate-internal modules of
//! the simulator. A daemon cannot see them, so a profile naming one must fail with a REASON rather
//! than "unknown strategy". [`SIMULATOR_ONLY`] carries that reason as data here, where every
//! consumer can read it; `vike-backtest`'s own registry test asserts the table names exactly its
//! retained arms, so the two cannot drift.
//!
//! ## The live-capability table
//! Resolving is not trading. Some portable strategies resolve fine and would then silently never
//! trade on a live mount — a funding reader with no live funding feed, and two-leg strategies with
//! no second mounted leg. [`LIVE_CAPABLE`] is the per-name verdict, one row per name with a written
//! reason for every "no", machine-checked exhaustive against [`PORTABLE_STRATEGIES`]. This is the
//! repo's per-venue capability-map playbook applied to strategies: declare today's reality, THEN
//! flip rows one at a time behind real evidence.
//!
//! ## The param-key table — because a params READER cannot fail
//! Every `from_params` here is a READER, not a schema: an unrecognised key is silently ignored and
//! the knob it meant to set stays at the strategy's COMPILED DEFAULT. That is fine for a backtest —
//! a wrong result is a result you look at — and it is NOT fine for a mount that signs real orders,
//! where a mistyped size knob is a position at a default size nobody typed. [`PARAM_KEYS`] declares,
//! per name, the keys its reader actually reads AND the TOML type each reader takes, so a consumer
//! can REFUSE the rest ([`unknown_params`]) and refuse a known key carrying an unusable value
//! ([`mistyped_params`]). ⚠ The two halves are the SAME defect: `and_then(Value::as_…)` yields
//! `None` for a wrong type exactly as it does for an absent key, so `size = "2"` — quoting a
//! number — mounts `size = 1` just as surely as `sizee = 2` does.
//! `crates/vike-strategy/tests/param_keys_gate.rs` scans the readers' own source in BOTH
//! directions, keys and types alike, so the table cannot drift away from what the code looks at. A
//! row may decline to enumerate ([`ParamKeys::NotEnumerated`]) — the A-S maker's ~60-key bag does —
//! but only with a written reason, and its consumer is then responsible for its own stricter rule.
//!
//! ## ...and [`PARAM_ROUTES`], because a key can be READ and still configure nothing
//! A key that names a ROUTE — `symbol`, `venue`, the `venues` table — is read by the strategy and
//! then OVERWRITTEN by the mount: `crates/vike-core/src/runtime/strategy_drive.rs`'s
//! `resolve_intent_symbol`/`resolve_intent_venue` stamp the MOUNT's own `(venue, symbol)` onto every
//! intent while `any_mount_multi` is false, and that is false on every mount this workspace builds
//! (`vike_run::MountSpec`'s `legs` is empty on every spec). So a params `symbol` that disagrees with
//! the mount passes [`unknown_params`], passes [`mistyped_params`], appears in [`resolved_params`]
//! — and the orders go somewhere else. [`PARAM_ROUTES`] declares which keys those are, per name, and
//! [`misrouted_params`] is the refusal that makes the state unreachable.
//!
//! ## ...and [`resolved_params`], because a type check only covers what it rejects
//! A reader may still CLAMP (`read_rungs`' `i.max(0)`), fall back on an unrecognised string
//! (`anchor` that is not `"fixed"` is `"first"`; `side` that is not `"short"`/`"sell"` is LONG),
//! drop a per-ROW value (`venues`' `filter_map`), or supply a default the profile never mentions
//! (`venue = "sim"`). Every one of those is well-typed input resolving to something the profile
//! does not say, so no key- or type-level rule can see it. [`resolved_params`] re-runs the reader
//! and reports what it LANDED ON, which is what a mount log must say: a log that repeats the
//! operator's table is a claim about the operator, and it is false in exactly the case worth
//! logging.
//!
//! ## ...and the CLASS-CLOSER, because a key can be RESOLVED and read by nobody
//! Five review rounds each found one more key that passed every rule above and still configured
//! nothing. They were not five bugs; they were one CLASS — **a key whose value the strategy HOLDS
//! but never READS, because another key in the same table sent the reader down a different branch**.
//! `anchor_price` is read only in `arm`'s `AnchorMode::Fixed` arm, so with `anchor` unset the number
//! resolves fine and the band of real limit orders is centred somewhere else; `tick` is consulted
//! only inside `bounded01` branches; and `trailing_scalper`'s two entry-timing gates and the two
//! market timestamps they read are pairwise inert (`entries_allowed` needs BOTH `> 0`).
//!
//! The thing that closes that class is a TEST, not a table:
//! `crates/vike-strategy/tests/param_gates.rs`'s `an_ungated_key_moves_the_strategy` drives every
//! enumerated strategy over a scripted market and fails, BY NAME, when a declared key does not
//! change the call trace — whether or not anybody thought to look for that key. [`PARAM_GATES`] is
//! that harness's own input: the declaration of which keys are legitimately read only under a
//! stated condition, so the harness knows what to exempt and can then prove each exemption in both
//! directions. ⚠ **It is TEST-ONLY DATA and nothing operator-facing reads it** — [`resolved_params`]
//! annotates nothing. Two rounds shipped an `(inert: …)` annotation on the mount line and each one
//! produced a fresh false claim in a configuration nobody had checked; [`PARAM_GATES`]' own doc
//! carries why that was deleted rather than repaired again.

use toml::Value;

use vike_model::{Bar, Broker, HftBroker, QuoteTick, SpreadModel, Strategy};

use crate::{
    AnchorMode, Controller, ControllerHarness, DcaAccumulate, FundingCapture,
    FundingCarryController, Grid, MomentumController, PairsZScore, TrailingScalper,
};
use vike_mm::SpreadMaker;

// The [`PARAM_KEYS`] table below is one row per key; spelling `ParamType::` on each of its ~70
// entries would bury the key names it exists to state. Imported unqualified HERE only.
use ParamType::{Bool, Integer, Number, Str, StrOrInteger, Table};

/// Every name [`strategy_by_name`] resolves. Kept in sync with that function's `match` by
/// `registry_lists_every_match_arm` below — the same construction `vike-backtest`'s roster used.
///
/// This is a SUBSET of `vike_backtest::harness::STRATEGIES` (which is this roster PLUS the
/// simulator-bound names in [`SIMULATOR_ONLY`]); the backtest roster stays the authority for what a
/// BACKTEST profile may name, and this one for what a LIVE mount may.
pub const PORTABLE_STRATEGIES: &[&str] = &[
    "buy_hold",
    "grid",
    "dca_accumulate",
    "spread_maker",
    "gueant_maker",
    "trailing_scalper",
    "momentum",
    "funding_carry",
    "funding_capture",
    "pairs_zscore",
];

/// The registry names that CANNOT leave `vike-backtest`, each with the reason — read by any
/// consumer that must explain a rejection (`vike-tradehub`'s profile validation) without depending
/// on the simulator to ask it.
///
/// Two distinct reasons, and the distinction matters: the first five are `impl Strategy<SimBroker>`
/// by DESIGN (their own module doc in `crates/vike-backtest/src/ref_strategies.rs` names the
/// simulator machinery each needs — the `SimBroker::symbols`/`schedule` FIELDS, the
/// `crate::sizing::PositionSizer` sentinel path, the cash-gate `weight` argument), so no registry
/// work makes them live. The last two are genuinely portable `impl<B: Broker>` strategies that
/// merely live in the simulator crate because they reach its crate-internal modules
/// (`crate::cheap_np_ask` / `crate::fair_value`); moving them is future work, not a law.
///
/// `crates/vike-backtest/src/harness/registry.rs`'s `simulator_only_table_names_the_retained_arms`
/// asserts this table is EXACTLY the set of arms that registry still owns.
pub const SIMULATOR_ONLY: &[(&str, &str)] = &[
    ("rotation_top_k", "simulator-bound: reads the SimBroker `symbols`/`schedule` fields directly"),
    (
        "bracket_per_symbol",
        "simulator-bound: measures the attached protective `stop` the SimBroker fill loop resolves",
    ),
    ("gated_weights", "simulator-bound: drives the SimBroker cash-gate `weight` submit argument"),
    (
        "caps_sizers_mask",
        "simulator-bound: drives `crate::sizing::PositionSizer` via SimBroker's raw=false submit \
         path (a 999.0 sentinel size)",
    ),
    (
        "tick_pair_mse",
        "simulator-bound: measures `SimBroker::position_of` against the engine's latency shadow",
    ),
    (
        "cheap_catch_updown_fair_value",
        "lives in the simulator crate: reaches its crate-internal `cheap_np_ask`/`fair_value` \
         modules (portable in principle, not yet moved)",
    ),
    (
        "sport_copy_follower",
        "lives in the simulator crate: reads a SIGNAL series the replay harness synthesizes \
         (portable in principle, not yet moved)",
    ),
];

/// Names that resolve in `vike-backtest`'s registry but are on NEITHER roster — the SCRIPT path.
///
/// `vike_backtest::harness::STRATEGIES` is the union of [`PORTABLE_STRATEGIES`] and
/// [`SIMULATOR_ONLY`] (its own `the_two_registry_halves_partition_the_roster` says so), and `rhai` is
/// DELIBERATELY absent from it: the arm needs a `src` param, so it is not default-resolvable and
/// every consumer that enumerates the roster and resolves each name with empty params would break.
/// That left a real hole in the three-rejection-classes story — a profile naming the script strategy
/// was told it did not EXIST, which is false and sends the operator hunting for a typo. This table
/// is the third row-set [`capability`] consults, so the answer is the true one.
///
/// ⚠ This table classifies the NAME, not the script path. Scripts themselves are LIVE-mountable
/// since `docs/decisions/0024-rhai-strategies-live.md`: `vike-tradehub` links `vike-script`
/// directly (a daemon sits far above both layer ranks) and mounts a script by PATH —
/// `[strategy] rhai = "<path>"` — never through this registry, whose row here stays true: the
/// NAME still resolves only where the inline-`src` arm lives.
///
/// `crates/vike-backtest/src/harness/registry.rs`'s
/// `script_only_names_resolve_there_and_not_here` is the two-direction gate.
pub const SCRIPT_ONLY: &[(&str, &str)] = &[(
    "rhai",
    "the SCRIPT path, by NAME: this arm compiles a `vike_script::RhaiStrategy` from an inline \
     `src` param and lives in `vike-backtest` (`vike-script` sits at the SAME layer rank as this \
     crate, so this crate cannot name it; the `src` requirement is why it is on no roster). \
     Scripts DO mount live — the daemon takes them by PATH, `[strategy] rhai = \"<path>\"`, \
     never through this registry.",
)];

/// Per-name LIVE-MOUNT verdict: `None` = mountable on the live core today, `Some(reason)` = it
/// RESOLVES but would not trade, with the reason.
///
/// ⚠ This table exists because the failure mode here is a SILENT NO-OP, not a compile error or a
/// panic: a strategy whose input never arrives mounts cleanly, logs nothing unusual, and simply
/// never submits. That is the live-vs-backtest divergence class this repo keeps a ledger of, and it
/// is precisely the class an operator discovers days later. Every "no" below names the missing
/// input, so flipping a row is a factual claim somebody can check rather than an opinion.
///
/// Exhaustive over [`PORTABLE_STRATEGIES`] by `live_capable_table_is_exhaustive`.
pub const LIVE_CAPABLE: &[(&str, Option<&str>)] = &[
    // Bars reaching a live mount ARE symbol-stamped — `crates/vike-core/src/runtime/mod.rs`'s
    // `BarClose` arm sets `bar.symbol = Some(key.1)` before `drive_strategy`, so a symbol-inferring
    // strategy routes to the mounted instrument rather than through `""`.
    ("buy_hold", None),
    ("grid", None),
    ("dca_accumulate", None),
    // Mountable live — but ⚠ NOT through THIS arm, and the difference used to be a live hazard.
    // `SpreadMaker::from_params` reads `[strategy.params]` and NOTHING else, so on a daemon whose
    // profile carries its own maker fields it would mount `qty = 1`, `tick_size = 0` and the
    // Bernoulli `[0,1]` wall clamp — the exact configuration that posted ZERO orders on a $64k
    // hyperliquid asset before `vike_run::MakerMountConfig::crypto` existed. So the daemon does not
    // use it: `vike_tradehub::config::DaemonProfile::resolve_strategy` builds these two names from
    // the profile's own maker fields through `vike_run::build_maker` — the SAME call the absent-
    // `[strategy]` default path makes — and REFUSES a `[strategy.params]` table outright, so the
    // named and the default spellings are ONE construction that cannot differ. This arm is the
    // BACKTEST's, where `[strategy.params]` is the only configuration that exists.
    ("spread_maker", None),
    ("gueant_maker", None),
    (
        "trailing_scalper",
        Some(
            "NAKED SHORT on its own venue: from flat it rests a SELL leg documented as the \
             single-token synthetic of `buy Down` (see its module doc), an equivalence that holds \
             in the simulator — where a signed position may go negative for free — and NOT on the \
             Polymarket CLOB it was written for, which requires the ERC-1155 outcome balance to \
             sell. Live from flat that leg cannot rest, so the strategy degrades to a one-sided \
             buyer while the backtest fills both sides; nothing in the mount path converts a sell \
             into the Down-buy it is priced as",
        ),
    ),
    ("momentum", None),
    (
        "funding_carry",
        Some(
            "TWO-LEG and cross-venue: needs one declared mount leg per carry venue, and no mount \
             surface declares legs today (`vike_run::MountSpec`'s `legs` is empty on every spec, \
             and the paper builder asserts it) — plus the same missing live funding series as \
             `funding_capture`",
        ),
    ),
    (
        "funding_capture",
        Some(
            "reads `Bar::funding`, which every live kline lane leaves `None` \
             (`vike_bridge_core::klines::kline_to_bar`) — it would mount and never see a funding \
             rate",
        ),
    ),
    (
        "pairs_zscore",
        Some(
            "TWO-LEG: needs `symbol_a` + `symbol_b` as declared mount legs admitted into the venue \
             engine's `extra_symbols`; a mount declares no legs today, so the second leg's bars \
             never arrive and its orders would be dropped at `ExecutionEngine::accepts_symbol`",
        ),
    ),
];

/// What this registry can say about a name — the ONE answer a consumer needs before mounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// Resolvable here AND mountable on a live core.
    Live,
    /// Resolvable here, but it would not trade on a live mount — the reason is from [`LIVE_CAPABLE`].
    NotLive(&'static str),
    /// Not in this registry: it belongs to the simulator crate — the reason is from
    /// [`SIMULATOR_ONLY`] or [`SCRIPT_ONLY`]. Both mean the same thing to a consumer that is about
    /// to mount ("it backtests; this binary does not link what runs it"), which is why they share
    /// one variant rather than splitting a distinction only `vike-backtest` can act on.
    SimulatorOnly(&'static str),
    /// No such strategy anywhere.
    Unknown,
}

/// The TOML value types a params key's READER actually takes — declared per key beside its name in
/// [`PARAM_KEYS`], because a params reader ignores a value of the wrong type exactly as silently as
/// it ignores a key it does not know.
///
/// ⚠ Every variant is named for what some reader in this crate DOES, not for what would be tidy.
/// [`Number`](ParamType::Number) is two TOML types because that is the workspace's lenient numeric
/// convention (`as_f64` = `as_float().or(as_integer())`, so `qty = 1` and `qty = 1.0` are the same
/// knob and a rule refusing the integer would break working profiles);
/// [`StrOrInteger`](ParamType::StrOrInteger) exists because `grid_dca`'s `read_side` really does
/// accept `side = "short"` OR `side = -1`. `crates/vike-strategy/tests/param_keys_gate.rs`'s
/// `every_declared_key_type_matches_its_reader` reads each reader's own accessor out of its source
/// and fails on any row that claims something else, in both directions — so a variant here is a
/// machine-checked claim about the code, never a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    /// `as_f64` — a TOML float OR integer.
    Number,
    /// `Value::as_integer` — a TOML integer only. A float is REFUSED: `rungs = 4.0` reads as
    /// nothing and the count silently stays at the compiled default.
    Integer,
    /// `Value::as_str`.
    Str,
    /// `Value::as_bool`.
    Bool,
    /// `Value::as_table` (the `[strategy.params.venues]` routing table).
    Table,
    /// `grid_dca::read_side` — a TOML string OR integer, each with its own meaning.
    StrOrInteger,
}

impl ParamType {
    /// The `toml::Value::type_str()` names this reader accepts. The gate compares this SET against
    /// the accessors it finds at the reader's own lookup sites, which is why it is spelled as the
    /// wire vocabulary rather than as Rust types.
    pub const fn accepted(&self) -> &'static [&'static str] {
        match self {
            ParamType::Number => &["float", "integer"],
            ParamType::Integer => &["integer"],
            ParamType::Str => &["string"],
            ParamType::Bool => &["boolean"],
            ParamType::Table => &["table"],
            ParamType::StrOrInteger => &["integer", "string"],
        }
    }

    /// Whether `v` is a value this key's reader will actually take.
    pub fn accepts(&self, v: &Value) -> bool {
        self.accepted().contains(&v.type_str())
    }

    /// How to say the requirement to an operator, in the vocabulary of the TOML they typed.
    pub const fn expected(&self) -> &'static str {
        match self {
            ParamType::Number => "a number (integer or float)",
            ParamType::Integer => "an integer",
            ParamType::Str => "a string",
            ParamType::Bool => "a boolean",
            ParamType::Table => "a table",
            ParamType::StrOrInteger => "a string or an integer",
        }
    }
}

/// What a name's `[strategy.params]` table may legally contain.
///
/// ⚠ The whole point is that a params READER cannot fail: every `from_params` in this workspace
/// ignores what it does not recognise, so `qtyy = 0.005` is not an error — it is `qty` at the
/// strategy's compiled default. In a backtest that is a wrong number on a chart. On a mount it is a
/// live order at a size nobody typed, behind at most the OPTIONAL `policy.max_notional_per_order`
/// ceiling and behind NOTHING when an operator has not set one.
///
/// ⚠ And the KEY is only half of it. `size = "2"` — quoting a number, the single most ordinary TOML
/// slip — is a key the reader knows carrying a value it cannot take, so `and_then(as_f64)` yields
/// `None` and the knob lands on the compiled default with the operator's own profile stating
/// otherwise. That is why a [`Declared`](ParamKeys::Declared) row carries a [`ParamType`] per key
/// and not just a name; [`mistyped_params`] is the reader for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKeys {
    /// Exactly the keys this name's reader looks at, each with the TOML type that reader accepts. A
    /// key outside this set changes nothing, and a declared key of the wrong type changes nothing
    /// either — so a consumer that cares (a live mount) can refuse both.
    Declared(&'static [(&'static str, ParamType)]),
    /// Deliberately NOT enumerated, with the reason. [`unknown_params`] then reports nothing, and
    /// the consumer owes its own stricter rule — see the `spread_maker` row.
    NotEnumerated(&'static str),
}

/// Per-name `[strategy.params]` key declaration, exhaustive over [`PORTABLE_STRATEGIES`]
/// (`param_keys_table_is_exhaustive`) and checked against the readers' own source, both directions,
/// by `crates/vike-strategy/tests/param_keys_gate.rs`.
///
/// Each row names the FILES its keys are read in, because that is what the gate scans: a key is
/// declared here only if some reader for that name really looks it up, and every key those readers
/// look up is declared by some name that reads in the same file.
///
/// The [`ParamType`] beside each key is the same claim about the same reader, one level finer: the
/// ACCESSOR it is looked up through. `Number` where the reader uses the lenient `as_f64`, `Integer`
/// where it insists on `Value::as_integer`, and so on — read off the source by the gate, never
/// chosen.
pub const PARAM_KEYS: &[(&str, ParamKeys)] = &[
    // `BuyHold::from_params` (this file).
    ("buy_hold", ParamKeys::Declared(&[("size", Number), ("symbol", Str)])),
    // `Grid::from_params` / `DcaAccumulate::from_params` (`grid_dca.rs`, incl. its `read_rungs` /
    // `read_side` helpers). ⚠ `rungs` is `Integer`, not `Number`: `read_rungs` is
    // `Value::as_integer`, so `rungs = 4.0` sets nothing.
    (
        "grid",
        ParamKeys::Declared(&[
            ("anchor", Str),
            ("anchor_price", Number),
            ("step", Number),
            ("rungs", Integer),
            ("size", Number),
            ("band", Number),
            ("bounded01", Bool),
            ("tick", Number),
            ("symbol", Str),
        ]),
    ),
    (
        "dca_accumulate",
        ParamKeys::Declared(&[
            // `read_side` takes EITHER spelling — `side = "short"` or `side = -1`.
            ("side", StrOrInteger),
            ("anchor", Str),
            ("anchor_price", Number),
            ("step", Number),
            ("rungs", Integer),
            ("size", Number),
            ("tp", Number),
            ("symbol", Str),
        ]),
    ),
    // ⚠ The A-S bag is ~60 keys spanning the whole Avellaneda–Stoikov/GLFT surface, documented as a
    // table on `vike_mm::SpreadMaker::from_params` — in ANOTHER crate. A hand copy of it here would
    // rot on the next knob, and this repo has the scars to prove it. It is not enumerated, and the
    // consumer that needed it does not need it: `vike_tradehub::config::DaemonProfile` REFUSES a
    // `[strategy.params]` table under these two names outright (it builds the maker from the
    // profile's own maker fields instead — see the `LIVE_CAPABLE` rows), which is strictly stronger
    // than key-checking. A backtest reads the bag through the arm above, where a wrong knob is a
    // wrong chart rather than a wrong order.
    (
        "spread_maker",
        ParamKeys::NotEnumerated(
            "the ~60-key A-S/GLFT bag is documented on `vike_mm::SpreadMaker::from_params`, in \
             another crate; a hand copy here would rot. The live consumer refuses this table \
             entirely and builds the maker from the profile's own maker fields",
        ),
    ),
    (
        "gueant_maker",
        ParamKeys::NotEnumerated("the `spread_maker` bag, forced to `SpreadModel::Gueant`"),
    ),
    // `TrailingScalper::from_params` (`trailing_scalper.rs`). ⚠ The four ms gates and
    // `exit_delay_ms` go through that reader's `i` closure (`Value::as_integer`), so an
    // `exit_delay_ms = 2_000.0` sets nothing; `qty`/`half_spread`/`profit_target` go through its
    // `f` closure and take either numeric spelling.
    (
        "trailing_scalper",
        ParamKeys::Declared(&[
            ("qty", Number),
            ("half_spread", Number),
            ("exit_delay_ms", Integer),
            ("profit_target", Number),
            ("entry_open_delay_ms", Integer),
            ("entry_cutoff_before_close_ms", Integer),
            ("market_open_ms", Integer),
            ("market_close_ms", Integer),
        ]),
    ),
    // The two CONTROLLERS: their own `from_params` + `barriers_from_params` (`controller.rs`) + the
    // harness-level knobs `controller_harness` reads (this file).
    (
        "momentum",
        ParamKeys::Declared(&[
            ("qty", Number),
            ("threshold", Number),
            ("tp", Number),
            ("sl", Number),
            ("time_limit_ms", Integer),
            ("trailing", Number),
            ("venue", Str),
            ("cooldown_ms", Integer),
            ("venues", Table),
        ]),
    ),
    (
        "funding_carry",
        ParamKeys::Declared(&[
            ("symbol", Str),
            ("qty", Number),
            ("tp", Number),
            ("sl", Number),
            ("time_limit_ms", Integer),
            ("trailing", Number),
            ("hold_periods", Number),
            ("entry_threshold", Number),
            ("venue", Str),
            ("cooldown_ms", Integer),
            ("venues", Table),
        ]),
    ),
    // `FundingCapture::from_params` (`funding_capture.rs`).
    (
        "funding_capture",
        ParamKeys::Declared(&[("threshold", Number), ("qty", Number), ("symbol", Str)]),
    ),
    // `PairsZScore::from_params` (`pairs.rs`). ⚠ `period` is a COUNT read through `as_f64` and then
    // rounded — so it is `Number` here, and `period = 2.6` is legal input that resolves to `3`.
    // That is a coercion a type check cannot see; the resolved echo ([`resolved_params`]) is what
    // makes it visible.
    (
        "pairs_zscore",
        ParamKeys::Declared(&[
            ("symbol_a", Str),
            ("symbol_b", Str),
            ("period", Number),
            ("entry_z", Number),
            ("exit_z", Number),
            ("beta", Number),
            ("notional", Number),
            ("taker_fee", Number),
            ("half_spread_bps", Number),
            ("hold_intervals", Number),
            ("funding_a", Number),
            ("funding_b", Number),
            ("max_half_life", Number),
        ]),
    ),
];

/// This name's [`PARAM_KEYS`] row, or `None` for a name this registry does not resolve.
pub fn param_keys(name: &str) -> Option<&'static ParamKeys> {
    PARAM_KEYS.iter().find(|(n, _)| *n == name).map(|(_, k)| k)
}

/// The keys in `params` that `name`'s reader will NOT look at — i.e. the ones that configure
/// nothing. Sorted, so an error message is stable.
///
/// Empty for an unknown name (nothing to say — [`capability`] already refuses it), for a
/// [`ParamKeys::NotEnumerated`] row (the consumer owes its own rule), and for a `params` value that
/// is not a table at all (`[strategy.params]` is a table by construction of the profile's serde
/// shape; a non-table carries no keys to judge).
pub fn unknown_params(name: &str, params: &Value) -> Vec<String> {
    let Some(ParamKeys::Declared(known)) = param_keys(name) else {
        return Vec::new();
    };
    let Some(table) = params.as_table() else {
        return Vec::new();
    };
    let mut unknown: Vec<String> =
        table.keys().filter(|k| !known.iter().any(|(n, _)| n == k)).cloned().collect();
    unknown.sort();
    unknown
}

/// One declared key whose VALUE is of a type its reader cannot take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamTypeError {
    /// The `[strategy.params]` key, exactly as the operator spelled it.
    pub key: String,
    /// What the reader takes, in TOML vocabulary — [`ParamType::expected`].
    pub expected: &'static str,
    /// What the profile handed it — `toml::Value::type_str()`.
    pub got: &'static str,
}

impl std::fmt::Display for ParamTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` wants {}, got {}", self.key, self.expected, self.got)
    }
}

/// The keys in `params` whose VALUE `name`'s reader cannot take — the type twin of
/// [`unknown_params`], and the half that catches the ordinary slip.
///
/// ⚠ A wrong TYPE fails exactly as silently as a wrong KEY, and by the same mechanism: every
/// accessor in this crate is an `and_then(Value::as_…)`, which yields `None` for a value of another
/// type just as it does for an absent one, so `.unwrap_or(default)` lands on the compiled default
/// either way. `size = "2"` — a quoted number, the most ordinary TOML slip there is — therefore
/// mounts `size = 1`. On a backtest that is a wrong chart; on this workspace's live mount it is an
/// order at a size nobody typed, which is the same consequence `unknown_params` exists for.
///
/// Empty for an unknown name, for a [`ParamKeys::NotEnumerated`] row and for a non-table `params` —
/// the same three abstentions as [`unknown_params`], for the same reasons. Keys ABSENT from the
/// table are not judged (absent is the default, which is legal); only a key that is PRESENT with an
/// unusable value is reported. Sorted by key, so a message is stable.
pub fn mistyped_params(name: &str, params: &Value) -> Vec<ParamTypeError> {
    let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
        return Vec::new();
    };
    let Some(table) = params.as_table() else {
        return Vec::new();
    };
    let mut bad: Vec<ParamTypeError> = declared
        .iter()
        .filter_map(|(key, ty)| {
            let v = table.get(*key)?;
            (!ty.accepts(v)).then(|| ParamTypeError {
                key: (*key).to_string(),
                expected: ty.expected(),
                got: v.type_str(),
            })
        })
        .collect();
    bad.sort_by(|a, b| a.key.cmp(&b.key));
    bad
}

/// What a declared [`PARAM_KEYS`] key NAMES when it names a ROUTE — WHERE an order goes — rather
/// than a knob of the strategy's own arithmetic.
///
/// The distinction is not taxonomy. A knob key is READ by the strategy and reaches the order it
/// produces; a route key is read by the strategy and then **OVERWRITTEN by the mount**, because the
/// live core stamps its own `(venue, symbol)` onto every intent a single-leg mount buffers. So a
/// route key is the one kind of declared key that can be well-typed, well-spelled, genuinely read —
/// and still configure nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// The INSTRUMENT the strategy stamps on its orders (the `symbol` argument of every
    /// [`vike_model::Broker`] submit verb).
    Symbol,
    /// The VENUE half of the `(venue, symbol)` pair the strategy keys its state under and echoes
    /// into the intents it produces.
    Venue,
    /// A `SYMBOL = "venue"` routing TABLE — one route per row.
    VenueMap,
}

/// Which of a name's declared keys name a ROUTE, and whether a SINGLE-LEG mount may check them.
///
/// ⚠ **This table exists because a route key is INERT on every mount this workspace builds, and
/// silently so.** `crates/vike-core/src/runtime/strategy_drive.rs`'s `resolve_intent_symbol` returns
/// the MOUNT's symbol unconditionally while `CoreThread::any_mount_multi` is false, and
/// `resolve_intent_venue` returns the MOUNT's venue on the same condition — and that condition is
/// false for every mount that exists, because `vike_run::MountSpec`'s `legs` is `Vec::new()` on
/// every spec (`MakerMountConfig::mount_spec` builds it, and `build_paper_strategy_core_with`
/// asserts it). So `[strategy.params] symbol = "OTHER"` on a mount of `"MOUNTED"` loads, reads,
/// type-checks, echoes as `symbol=OTHER` — and the orders go to `"MOUNTED"`.
///
/// That is the settings-CONSUMPTION defect this repo deletes keys for: a declared-but-unconsumed
/// key "hands the operator positive confirmation of something false", and `Policy::max_total_exposure`
/// was deleted for exactly that. It is WORSE here than for a size knob, because the knob names which
/// instrument real orders go to. [`misrouted_params`] is the reader that makes it unreachable:
/// a consumer refuses a route key that does not name the mount's OWN route.
///
/// Exhaustive over [`PORTABLE_STRATEGIES`] by `param_routes_table_is_exhaustive`, and every key
/// named below is a declared [`PARAM_KEYS`] key of the same name and type
/// (`every_route_key_is_a_declared_key_of_the_right_type`), so a row cannot name a key no reader
/// reads. The reverse direction — a route-shaped declared key with no row — is
/// `every_route_shaped_declared_key_has_a_route_row`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamRoutes {
    /// SINGLE-LEG: every route key below names the ONE market this strategy trades, so a
    /// single-symbol mount can compare each against its own `(venue, symbol)` — and must, because
    /// it would otherwise override it. An EMPTY slice means the name has no route key at all.
    SingleLeg(&'static [(&'static str, RouteKind)]),
    /// MULTI-LEG: these keys name LEGS — markets that are not the mount's own — so "must equal the
    /// mount" is FALSE about them and [`misrouted_params`] declines to judge them. The reason is
    /// data. ⚠ This is not a licence to mount one: every multi-leg name is a
    /// [`Capability::NotLive`] row in [`LIVE_CAPABLE`] naming the same missing mount legs, and a
    /// consumer checks capability FIRST — so today a multi-leg name is refused by NAME, one step
    /// before its params are looked at.
    MultiLeg(&'static [(&'static str, RouteKind)], &'static str),
    /// Deliberately NOT enumerated, mirroring this name's [`ParamKeys::NotEnumerated`] row — the key
    /// set is unknown, so its route subset is too. [`misrouted_params`] reports nothing and the
    /// consumer owes its own stricter rule.
    NotEnumerated(&'static str),
}

/// Per-name ROUTE-key declaration — see [`ParamRoutes`] for why a route key is the one declared key
/// that can be read and still configure nothing.
pub const PARAM_ROUTES: &[(&str, ParamRoutes)] = &[
    ("buy_hold", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    ("grid", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    ("dca_accumulate", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    (
        "spread_maker",
        ParamRoutes::NotEnumerated(
            "mirrors this name's `ParamKeys::NotEnumerated` row — the ~60-key A-S bag is not \
             enumerated here, so its route subset cannot be either. The live consumer refuses the \
             whole table, which is strictly stronger",
        ),
    ),
    (
        "gueant_maker",
        ParamRoutes::NotEnumerated("the `spread_maker` bag, forced to `SpreadModel::Gueant`"),
    ),
    // No route key at all: every declared key is a size/price/time knob, and the instrument comes
    // from the bar/tick the harness dispatched on.
    ("trailing_scalper", ParamRoutes::SingleLeg(&[])),
    // ⚠ NO `symbol` key — this one routes purely off `bar.symbol`/`tick.symbol`. Its route keys are
    // the two VENUE knobs, and they are route keys for the same reason a `symbol` is: the harness
    // keys its executors under `(venue_for(symbol), symbol)` and echoes that venue into the
    // `PositionIntent` it produces (`crates/vike-strategy/src/controller.rs`'s `maybe_open`), and
    // the live core then discards it for the mount's own (`resolve_intent_venue`).
    //
    // ⚠ DECLARED RESIDUAL — an ABSENT `venue` is NOT covered, and cannot be. `harness_venue`'s
    // fallback is the literal `"sim"`, so a hyperliquid mount that sets no `venue` runs a harness
    // labelled `sim` and [`resolved_params`] echoes `venue=sim`. That value is TRUE about the
    // harness (it really is the field `ControllerHarness::venue` holds, and
    // `the_echo_reports_the_resolution_and_not_the_input` pins it deliberately) and it is not an
    // order destination: `Broker::submit_market`/`submit_limit` take no venue at all, so the label
    // never reaches an order — `the_harness_venue_is_a_label_and_not_an_order_destination` drives
    // that rather than asserting it. Nothing here can refuse a key the operator did not write; what
    // this row buys is that a venue they DO write must be the one they are mounted on.
    (
        "momentum",
        ParamRoutes::SingleLeg(&[("venue", RouteKind::Venue), ("venues", RouteKind::VenueMap)]),
    ),
    (
        "funding_carry",
        ParamRoutes::MultiLeg(
            &[
                ("symbol", RouteKind::Symbol),
                ("venue", RouteKind::Venue),
                ("venues", RouteKind::VenueMap),
            ],
            "TWO-LEG and cross-venue by construction: `venues` is the per-leg venue map its carry \
             book is built from (≥2 venues, or there is no carry to open) and an EMPTY `symbol` is \
             its REAL two-leg mode, so no value of these keys is required to name the mount's own \
             single market. `LIVE_CAPABLE` refuses the name for the same missing mount legs",
        ),
    ),
    ("funding_capture", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    (
        "pairs_zscore",
        ParamRoutes::MultiLeg(
            &[("symbol_a", RouteKind::Symbol), ("symbol_b", RouteKind::Symbol)],
            "TWO-LEG: the two keys name the two legs of one spread trade, so at most ONE of them \
             could ever equal a single-leg mount's symbol and requiring both to would be \
             incoherent. `LIVE_CAPABLE` refuses the name for the missing second mount leg",
        ),
    ),
];

/// This name's [`PARAM_ROUTES`] row, or `None` for a name this registry does not resolve.
pub fn param_routes(name: &str) -> Option<&'static ParamRoutes> {
    PARAM_ROUTES.iter().find(|(n, _)| *n == name).map(|(_, r)| r)
}

/// The CONDITION under which a declared [`PARAM_KEYS`] key's value is actually CONSUMED — a
/// predicate over the SAME resolved rows [`resolved_params`] reports, so a gate and the echo can
/// never disagree about what the OTHER keys landed on.
///
/// ⚠ **This is a claim about ANOTHER key, never about this one's own value.** A key whose own value
/// selects a branch (`profit_target > 0` picks the fixed-target exit, `max_half_life <= 0.0`
/// disables the regime gate) is CONSUMED in both branches — the reader looked at it and acted on
/// what it found, which is the opposite of the defect this table exists for. Only a key the reader
/// HOLDS and never looks at, because some OTHER key sent it down a different path, gets a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Consumed only while `key` RESOLVED to one of these echo values (`anchor` ⇒ `"fixed"`,
    /// `bounded01` ⇒ `"true"`). Compared against the RESOLUTION, not the input, so
    /// `anchor = "fixd"` — which the reader silently reads as `first` — closes the gate as surely
    /// as an absent `anchor` does.
    Is(&'static str, &'static [&'static str]),
    /// Consumed only while `key`'s resolved value parses as a number strictly greater than zero.
    /// Every reader this covers gates on `> 0` literally — `trailing_scalper.rs`'s `entries_allowed`
    /// is `self.entry_open_delay_ms > 0 && self.market_open_ms > 0`.
    Positive(&'static str),
    /// Consumed only while EVERY listed sub-gate holds — the shape a condition over more than one
    /// other key needs, and the only one that reports ALL the failing conjuncts at once.
    ///
    /// ⚠ **No [`PARAM_GATES`] row constructs it today**, and that is a deletion rather than an
    /// oversight: the six degenerate-ladder rows that did were removed in round 7 (see that table's
    /// own doc). It stays because it is the combinator a multi-key condition takes, `unmet`/`keys`
    /// reach through it, and `the_gate_predicate_can_actually_fail` exercises it — not because
    /// anything claims it is in use.
    All(&'static [Gate]),
}

impl Gate {
    /// The sub-gates that FAIL against `rows` (a [`resolved_params`] row list), each rendered as a
    /// `key=value` naming the other key and what it resolved to. EMPTY ⇒ the key really is
    /// consumed.
    ///
    /// A gate naming a key the rows do not carry renders `key=(absent)` and counts as unmet —
    /// unreachable in a green tree (`every_gate_names_a_declared_key_of_the_same_strategy` pins it),
    /// and a loud wrong answer beats a silent "consumed" if it ever became reachable.
    pub fn unmet(&self, rows: &[(&'static str, String)]) -> Vec<String> {
        fn value<'a>(rows: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
            rows.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
        }
        fn missing(key: &str) -> Vec<String> {
            vec![format!("{key}=(absent)")]
        }
        match self {
            Gate::Is(key, wanted) => match value(rows, key) {
                Some(v) if wanted.contains(&v) => Vec::new(),
                Some(v) => vec![format!("{key}={v}")],
                None => missing(key),
            },
            Gate::Positive(key) => match value(rows, key) {
                Some(v) if v.parse::<f64>().is_ok_and(|n| n > 0.0) => Vec::new(),
                Some(v) => vec![format!("{key}={v}")],
                None => missing(key),
            },
            Gate::All(gates) => gates.iter().flat_map(|g| g.unmet(rows)).collect(),
        }
    }

    /// Every key this gate READS — the reverse edge `every_gate_names_a_declared_key_of_the_same_
    /// strategy` walks, so a gate cannot name a knob that is not one of the same strategy's
    /// declared keys.
    pub fn keys(&self) -> Vec<&'static str> {
        match self {
            Gate::Is(key, _) | Gate::Positive(key) => vec![key],
            Gate::All(gates) => gates.iter().flat_map(Gate::keys).collect(),
        }
    }
}

/// Which declared keys are read CONDITIONALLY, and on what — one row per `(strategy, key)`.
///
/// ⚠ **This is TEST-ONLY DATA, and its one consumer is the class-closer.**
/// `crates/vike-strategy/tests/param_gates.rs` drives every enumerated strategy over a scripted
/// market and demands that each declared key CHANGE the call trace, so a key that configures
/// nothing fails by name whether or not anybody went looking for it. A key that is legitimately
/// read only under some OTHER key would fail that demand for a reason that is not a defect — a row
/// here is the declaration of that condition, and it buys the key an exemption the harness then
/// proves in both directions (inert while the condition is unmet, live once it is met). Nothing
/// operator-facing reads this table: [`resolved_params`] annotates nothing.
///
/// The class it exempts from is real, and it is why the harness exists. Five review rounds each
/// found one more instance of the same shape: a `[strategy.params]` key that is spelled right
/// ([`unknown_params`]), typed right ([`mistyped_params`]), routed right ([`misrouted_params`]) and
/// genuinely read by `from_params` — and whose value the strategy then never LOOKS AT, because
/// another key in the same table sent it down a different branch.
///
/// The instances, and where each one lives:
///
/// | key | inert while | the reader |
/// |---|---|---|
/// | `grid`/`dca_accumulate` `anchor_price` | `anchor` ≠ `fixed` | `grid_dca.rs`'s `Grid::anchor_at` and `DcaAccumulate::arm` read it only in the `AnchorMode::Fixed` arm; ANY unrecognised `anchor` spelling falls to `FirstPrice` |
/// | `grid` `tick` | `bounded01` = false | `grid_dca.rs`'s `on_grid` is `!self.bounded01 \|\| …`, and both band clamps in `Grid::arm` sit inside `if self.bounded01` |
/// | `trailing_scalper` `entry_open_delay_ms` ⇄ `market_open_ms` | the OTHER of the pair ≤ 0 | `trailing_scalper.rs`'s `entries_allowed` gates on `self.entry_open_delay_ms > 0 && self.market_open_ms > 0` — either alone is inert |
/// | `trailing_scalper` `entry_cutoff_before_close_ms` ⇄ `market_close_ms` | the OTHER of the pair ≤ 0 | the same `entries_allowed`, its second conjunction |
///
/// ⚠ **A row is BEHAVIOURALLY PROVEN, not read off the prose above.**
/// `crates/vike-strategy/tests/param_gates.rs` drives each strategy over a scripted market twice —
/// once per contrasting value of the key — and asserts the two runs are call-for-call IDENTICAL
/// while the gate is unmet and DIFFERENT once it is met. The same harness demands a row for any
/// declared key that turns out inert at its strategy's default table, which is what makes this a
/// closed class rather than a sixth patch.
///
/// ## ⚠ This table used to feed the mount echo. That is DELETED, not repaired again
///
/// Rounds 6 and 7 rendered a gated row on the daemon's mount line as
/// `anchor_price=60000 (inert: anchor=first)`. Round 6 added the marking and, in the same round,
/// found it PARTIAL at `rungs = 0` — a configuration where both `arm`s return before resting
/// anything, so every key of both strategies is inert while only two carried a marker, from which
/// an operator reasonably concludes the unmarked ones are in force. Round 7 deleted six rows for
/// that, and then found the SURVIVING rows print bare in exactly the same dead configuration (an
/// armed `anchor = "fixed"` satisfies its own gate while the ladder reads neither value), which
/// made four prose claims about the marking false as shipped.
///
/// Every round the marking produced a NEW false claim in a configuration nobody had checked, and
/// each fix MOVED the falsehood rather than removing it. The reason is structural, not a matter of
/// more rows: **a marker is a positive claim about a key, and making positive claims correctly
/// across every combination of every other key is a static analysis, not a documentation feature.**
/// The harness's own residual (`the_shapes_this_harness_cannot_see`) already carries executed cases
/// no predicate over the params table can express — inertness that depends on the FEED, inertness
/// only at a non-base combination, and the degenerate ladder itself.
///
/// So [`resolved_params`] reports what each knob RESOLVED TO and claims nothing about what is in
/// force, and the class stays closed by the test rather than by an annotation.
///
/// ## Why an inert key is not REFUSED either, when a misrouted one is refused
///
/// [`misrouted_params`] refuses, because a `symbol` naming a market the mount does not trade is
/// wrong under EVERY configuration of the rest of the table — there is no value of any other key
/// that makes it right, and the consequence (orders on another instrument) is unrecoverable at
/// runtime. An inert key is the opposite on both counts:
///
///   - **It is armed from the SAME table.** `anchor_price = 60000` is exactly right the moment
///     `anchor = "fixed"` appears one line above it; the profile is incomplete, not wrong.
///   - **Refusing would reject a shipped profile shape.** `crates/vike-strategy/src/
///     trailing_scalper.rs`'s module doc says the batch tool supplies `market_open_ms` /
///     `market_close_ms` per run *while the delay/cutoff knobs stay off by default* — i.e. the
///     ordinary run has half of an inert pair set, deliberately. A refusal would make the two
///     entry-timing features un-shippable in their own documented default state.
pub const PARAM_GATES: &[(&str, &str, Gate)] = &[
    ("grid", "anchor_price", Gate::Is("anchor", &["fixed"])),
    ("grid", "tick", Gate::Is("bounded01", &["true"])),
    ("dca_accumulate", "anchor_price", Gate::Is("anchor", &["fixed"])),
    ("trailing_scalper", "entry_open_delay_ms", Gate::Positive("market_open_ms")),
    ("trailing_scalper", "market_open_ms", Gate::Positive("entry_open_delay_ms")),
    ("trailing_scalper", "entry_cutoff_before_close_ms", Gate::Positive("market_close_ms")),
    ("trailing_scalper", "market_close_ms", Gate::Positive("entry_cutoff_before_close_ms")),
];

/// This `(name, key)`'s [`PARAM_GATES`] row, or `None` when the key is read unconditionally.
pub fn param_gate(name: &str, key: &str) -> Option<&'static Gate> {
    PARAM_GATES.iter().find(|(n, k, _)| *n == name && *k == key).map(|(_, _, g)| g)
}

/// One route key whose value names a market the mount does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMismatch {
    /// The `[strategy.params]` key, exactly as the operator spelled it — or `venues.<SYMBOL>` for
    /// one ROW of a routing table.
    pub key: String,
    /// What that value NAMES, in the operator's own vocabulary.
    pub named: String,
    /// What this mount actually routes to.
    pub mounted: String,
}

impl std::fmt::Display for RouteMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` names {}, but this mount routes {}", self.key, self.named, self.mounted)
    }
}

/// The route keys in `params` that name a market OTHER than the single-leg mount's own
/// `(venue, symbol)` — i.e. the ones the mount would silently override.
///
/// The third sibling of [`unknown_params`] (a key no reader reads) and [`mistyped_params`] (a key
/// whose value no reader takes). This one is the case both of those pass: the key is read, the value
/// is well-typed, and the mount overwrites it anyway. See [`ParamRoutes`] for the mechanism and for
/// why that is the settings-consumption defect rather than a tidiness question.
///
/// Empty for an unknown name, for a [`ParamRoutes::NotEnumerated`] row, for a
/// [`ParamRoutes::MultiLeg`] name (its keys name legs, not this mount's market — the consumer
/// refuses those names outright, one step earlier) and for a non-table `params`.
///
/// Two states of a `Symbol` key are deliberately NOT reported, because neither is a route this mount
/// cannot take: ABSENT is the working default (the strategy takes its symbol off the feed, which the
/// runtime stamps with the mount's own — `crates/vike-core/src/runtime/mod.rs`'s `BarClose` arm), and
/// EMPTY is a mount that cannot trade at all, which [`resolved_params`]' `opt_sym` already reports as
/// such. A value of the wrong TYPE is [`mistyped_params`]' finding, not this one — a consumer runs
/// that check first so the operator gets the type sentence rather than a confusing route sentence.
///
/// A `VenueMap` ROW is legal only when it names this mount's own route outright
/// (`<mount symbol> = "<mount venue>"`), because a row for any other symbol never matches the one
/// series this mount receives and a row for any other venue is discarded by `resolve_intent_venue`.
/// A row whose VALUE is not a string is reported too: `harness_venue_map`'s `filter_map` drops it,
/// so it is a route the operator wrote and the reader never even built.
pub fn misrouted_params(
    name: &str,
    params: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<RouteMismatch> {
    let Some(ParamRoutes::SingleLeg(routes)) = param_routes(name) else {
        return Vec::new();
    };
    let Some(table) = params.as_table() else {
        return Vec::new();
    };
    let mut out: Vec<RouteMismatch> = Vec::new();
    for (key, kind) in routes.iter() {
        let Some(v) = table.get(*key) else {
            continue;
        };
        match kind {
            RouteKind::Symbol => {
                if let Some(s) = v.as_str()
                    && !s.is_empty()
                    && s != symbol
                {
                    out.push(RouteMismatch {
                        key: (*key).to_string(),
                        named: format!("instrument {s:?}"),
                        mounted: format!("{symbol:?}"),
                    });
                }
            }
            RouteKind::Venue => {
                if let Some(s) = v.as_str()
                    && s != venue
                {
                    out.push(RouteMismatch {
                        key: (*key).to_string(),
                        named: format!("venue {s:?}"),
                        mounted: format!("{venue:?}"),
                    });
                }
            }
            RouteKind::VenueMap => {
                if let Some(rows) = v.as_table() {
                    for (row_symbol, row_value) in rows.iter() {
                        if row_symbol == symbol && row_value.as_str() == Some(venue) {
                            continue;
                        }
                        let named = match row_value.as_str() {
                            Some(row_venue) => format!("{row_symbol:?} on venue {row_venue:?}"),
                            None => format!(
                                "{row_symbol:?} on a {} (which this reader DROPS)",
                                row_value.type_str()
                            ),
                        };
                        out.push(RouteMismatch {
                            key: format!("{key}.{row_symbol}"),
                            named,
                            mounted: format!("{symbol:?} on venue {venue:?}"),
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The WHOLE-TABLE verdict the three key-level readers above cannot reach: a params table under
/// which `name` rests NO order at all — on any market, at any price — with the reason, or `None`
/// when the mount can trade.
///
/// The fourth sibling of [`unknown_params`] (a key no reader reads), [`mistyped_params`] (a key
/// whose value no reader takes) and [`misrouted_params`] (a key the mount overrides). It is the
/// case all three of those PASS: every key is spelled right, typed right and routed right, and the
/// ladder they describe between them is empty. Both instances were MEASURED before this existed and
/// are recorded as such in `crates/vike-strategy/tests/param_gates.rs`'s `DEAD` ledger — a
/// `dca_accumulate` whose `anchor = "fixed"` left `anchor_price` at its compiled `0`, and a `grid`
/// whose `bounded01` left `step` at its compiled `1.0`, one rung spacing spanning the entire 0..1
/// domain. Each one loads, mounts, echoes a clean configuration line and then never submits.
///
/// ## Why this refuses where an INERT KEY is deliberately tolerated
///
/// [`PARAM_GATES`]' doc argues at length that an inert key must NOT be refused, and both halves of
/// that argument invert here:
///
///   - **It is not armed from the same table.** `anchor_price = 60000` becomes right the moment
///     `anchor = "fixed"` is added above it — the profile is incomplete, not wrong. An empty ladder
///     has no missing line: every key is present, and no value of any OTHER key rescues it. The
///     anchor is fixed by the params or bounded by the market's own walls, and `arm` runs once,
///     from the params, at the first event.
///   - **It rejects no shipped profile shape.** The tolerated inert keys are a documented default
///     state (`crates/vike-strategy/src/trailing_scalper.rs`'s batch tool ships half of an inert
///     pair set, deliberately). Nothing ships an empty ladder: the profiles that do exist
///     (`crates/vike-backtest/profiles/wf_grid.toml`,
///     `crates/vike-backtest/profiles/wf_dca_accumulate.toml`) rest rungs.
///
/// And the consequence is the one this daemon's own config doc calls the class worth failing loudly
/// for: it mounts cleanly, logs nothing unusual and simply never submits — discovered days later.
///
/// ## What it deliberately does NOT ask
///
/// Not "would this mount place an order". That is a question about the PRICE PATH, and a strategy
/// that legitimately waits for a market condition — `trailing_scalper` outside its entry window,
/// `momentum` under its threshold — answers it identically to a dead one. No load-time check can
/// separate those without simulating a market, and the verdict would then be about the market. The
/// decidable question is the narrower one: **does the order set this table describes contain
/// anything at all**, which for these two names is fixed at load because `arm` builds its whole
/// ladder from the params plus one anchor and never rebuilds it from nothing.
///
/// That is also why the answer is computed by [`Grid::arms_no_rung`] / [`DcaAccumulate::arms_no_rung`]
/// — the ladder builders themselves — rather than re-derived here from the key values: a predicate
/// that restates a reader is a predicate that rots away from it.
///
/// `None` for every other name, and that is a statement rather than a default: these two are the
/// only strategies whose entire order set is decided at load. Every other name's order flow is a
/// function of the market it is fed, so "rests nothing" is not a load-time question for it — adding
/// one that IS is adding an arm here.
///
/// ⚠ **The rule is EMPTINESS, never a suspicious value, and the boundary is deliberate in both
/// directions.** A `dca_accumulate` SHORT ladder at `anchor_price = 0` rests `step`, `2·step`, …
/// and loads: refusing the value `0` would be an over-refusal of a coherent configuration. And a
/// `grid` at `anchor_price = 0` with `bounded01` off rests its rungs at NEGATIVE prices and also
/// loads — absurd, but a different defect (orders nobody can fill, not an absence of orders), and
/// this predicate deliberately says nothing about it rather than growing a second rule inside the
/// first.
///
/// ⚠ **It is a MOUNT rule, not a reader rule.** Nothing calls it from [`strategy_by_name`]: a
/// backtest sweep gridding over `rungs`/`step` may legitimately visit a degenerate corner and get a
/// flat equity curve for it, and refusing there would break the sweep this strategy exists to feed.
/// The consumer is the daemon's profile validation.
pub fn unarmable_params(name: &str, params: &Value) -> Option<String> {
    let empty = match name {
        "grid" => Grid::from_params(params).arms_no_rung(),
        "dca_accumulate" => DcaAccumulate::from_params(params).arms_no_rung(),
        _ => return None,
    };
    if !empty {
        return None;
    }
    // The reason is the RESOLUTION, not a re-derivation of which knob is at fault: `resolved_params`
    // is gated to report exactly what the readers landed on, so the line shows `anchor_price=0` or
    // `step=1 bounded01=true` in the operator's own vocabulary without this function claiming to
    // know which one they meant to type.
    let echo = resolved_params(name, params)
        .map(|rows| rows.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    Some(format!(
        "`{name}` rests NO rung at any anchor these params permit, so this mount can never place an \
         order — on any market, at any price. It resolved to: {echo}"
    ))
}

/// What `name`'s reader ACTUALLY resolved out of `params` — every declared key with the value the
/// strategy will run with, in [`PARAM_KEYS`] order. `None` for a name with no enumerated key set
/// (the two maker aliases) or no row at all.
///
/// ⚠ This is the honest half of a mount log, and it is not the same thing as echoing the profile.
/// A consumer that prints the TOML table prints what was TYPED, which is a claim about the operator
/// rather than about the daemon, and it is false whenever the two differ — which is precisely the
/// case worth logging. [`mistyped_params`] removes one cause of divergence, and only one: a reader
/// may still CLAMP (`read_rungs`' `i.max(0)`, `PairsZScore`'s `period.round().max(2.0)`), fall back
/// on an unrecognised string (`anchor` that is not `"fixed"` is `"first"`; `side` that is not
/// `"short"`/`"sell"` is LONG), or supply a default the profile never mentions (`venue = "sim"` on
/// a controller harness). Every one of those is type-correct input resolving to something the
/// profile does not say, and every one of them shows up HERE.
///
/// It re-runs the SAME `from_params` the mount used rather than re-deriving anything: these readers
/// are pure functions of the table, so the values cannot disagree with the mounted ones. What could
/// drift is WHICH knobs get reported, and that is gated —
/// `resolved_params_reports_exactly_the_declared_keys_in_order` pins the output keys to
/// [`PARAM_KEYS`], so a knob added to a reader lands in the echo or fails CI.
///
/// ⚠ **It reports what each knob RESOLVED TO. It claims nothing about what is IN FORCE.** Whether a
/// resolved value is then CONSUMED can depend on the other knobs and on the market the mount is fed.
///
/// ⚠ **No example belongs in this paragraph.** An example of an inert key IS a positive claim about
/// consumption — the very thing this line refuses to make — and it is checked by nobody, so it rots
/// into a falsehood the moment a reader changes. This doc carried two such examples and a reviewer
/// measured one of them false. The authority on which knob a reader consults is the reader; the
/// authority on a knob nothing reads AT ALL is `crates/vike-strategy/tests/param_gates.rs`, which
/// drives every strategy and fails when a declared key does not move it.
///
/// Two rounds annotated the rows a predicate over this table could reach, and each round's
/// annotation turned out to be a fresh false claim in a configuration nobody had checked —
/// [`PARAM_GATES`]' own doc carries that history.
pub fn resolved_params(name: &str, params: &Value) -> Option<Vec<(&'static str, String)>> {
    fn num(v: f64) -> String {
        format!("{v}")
    }
    fn opt_num(v: Option<f64>) -> String {
        v.map(num).unwrap_or_else(|| "(unarmed)".to_string())
    }
    fn opt_ms(v: Option<i64>) -> String {
        v.map(|i| i.to_string()).unwrap_or_else(|| "(unarmed)".to_string())
    }
    /// An OPTIONAL symbol, in the THREE states it actually has — absent, explicitly EMPTY, and set.
    ///
    /// ⚠ **Absent and empty are not the same mount, and rendering them alike is the same defect
    /// [`req_sym`] below was fixed for.** `None` is the working default: the strategy takes its
    /// symbol off the feed. `Some("")` is a value the operator STATED, and every reader this helper
    /// serves refuses to trade on it — `BuyHold::buy` returns on `symbol.is_empty()`,
    /// `crates/vike-strategy/src/funding_capture.rs`'s `on_bar` returns with the comment "SimBroker
    /// panics on an empty symbol — never route through", and BOTH of
    /// `crates/vike-strategy/src/grid_dca.rs`'s `drive` methods open with the same guard. The test
    /// `the_empty_symbol_this_echo_reports_really_does_stop_the_strategy` proves that rather than
    /// asserting it. Echoing `""` shows a mount that cannot trade as one that is merely taking its
    /// symbol from the feed.
    ///
    /// It mirrors the readers' own `is_empty()` test and not a stricter one (no trimming): a
    /// whitespace symbol PASSES their guard and IS routed, so calling that one unset here would be
    /// the same lie pointing the other way.
    fn opt_sym(v: &Option<String>) -> String {
        match v.as_deref() {
            None => "(from the feed)".to_string(),
            Some("") => "(empty — this strategy cannot trade)".to_string(),
            Some(s) => s.to_string(),
        }
    }
    /// A REQUIRED symbol left empty. ⚠ Not cosmetic: `PairsZScore` with either leg unset never
    /// routes an order (there is no single-symbol fallback for a two-leg trade), so an echo that
    /// printed an empty string would show a mount that cannot trade as if it were configured.
    fn req_sym(v: &str) -> String {
        if v.is_empty() { "(unset — this leg cannot route)".to_string() } else { v.to_string() }
    }
    fn anchor(m: AnchorMode) -> String {
        match m {
            AnchorMode::Fixed => "fixed",
            AnchorMode::FirstPrice => "first",
        }
        .to_string()
    }
    fn venues(map: &[(String, String)]) -> String {
        if map.is_empty() {
            return "(none)".to_string();
        }
        map.iter().map(|(s, v)| format!("{s}:{v}")).collect::<Vec<_>>().join(",")
    }

    Some(match name {
        "buy_hold" => {
            let s = BuyHold::from_params(params);
            vec![("size", num(s.size)), ("symbol", opt_sym(&s.symbol))]
        }
        "grid" => {
            let g = Grid::from_params(params);
            vec![
                ("anchor", anchor(g.anchor_mode)),
                ("anchor_price", num(g.anchor_price)),
                ("step", num(g.step)),
                ("rungs", g.rungs.to_string()),
                ("size", num(g.size)),
                ("band", num(g.band)),
                ("bounded01", g.bounded01.to_string()),
                ("tick", num(g.tick)),
                ("symbol", opt_sym(&g.symbol)),
            ]
        }
        "dca_accumulate" => {
            let d = DcaAccumulate::from_params(params);
            vec![
                ("side", if d.side < 0 { "short" } else { "long" }.to_string()),
                ("anchor", anchor(d.anchor_mode)),
                ("anchor_price", num(d.anchor_price)),
                ("step", num(d.step)),
                ("rungs", d.rungs.to_string()),
                ("size", num(d.size)),
                ("tp", num(d.tp)),
                ("symbol", opt_sym(&d.symbol)),
            ]
        }
        "trailing_scalper" => {
            let t = TrailingScalper::from_params(params);
            vec![
                ("qty", num(t.qty)),
                ("half_spread", num(t.half_spread)),
                ("exit_delay_ms", t.exit_delay_ms.to_string()),
                ("profit_target", num(t.profit_target)),
                ("entry_open_delay_ms", t.entry_open_delay_ms.to_string()),
                ("entry_cutoff_before_close_ms", t.entry_cutoff_before_close_ms.to_string()),
                ("market_open_ms", t.market_open_ms.to_string()),
                ("market_close_ms", t.market_close_ms.to_string()),
            ]
        }
        "momentum" => {
            let c = MomentumController::from_params(params);
            let b = c.barriers;
            vec![
                ("qty", num(c.qty)),
                ("threshold", num(c.threshold)),
                ("tp", opt_num(b.take_profit)),
                ("sl", opt_num(b.stop_loss)),
                ("time_limit_ms", opt_ms(b.time_limit_ms)),
                ("trailing", opt_num(b.trailing)),
                ("venue", harness_venue(params).to_string()),
                ("cooldown_ms", harness_cooldown_ms(params).to_string()),
                ("venues", venues(&harness_venue_map(params))),
            ]
        }
        "funding_carry" => {
            let c = FundingCarryController::from_params(params);
            let b = c.barriers();
            vec![
                (
                    "symbol",
                    if c.symbol().is_empty() {
                        "(both legs)".to_string()
                    } else {
                        c.symbol().to_string()
                    },
                ),
                ("qty", num(c.qty())),
                ("tp", opt_num(b.take_profit)),
                ("sl", opt_num(b.stop_loss)),
                ("time_limit_ms", opt_ms(b.time_limit_ms)),
                ("trailing", opt_num(b.trailing)),
                ("hold_periods", num(c.hold_periods())),
                ("entry_threshold", num(c.entry_threshold())),
                ("venue", harness_venue(params).to_string()),
                ("cooldown_ms", harness_cooldown_ms(params).to_string()),
                ("venues", venues(&harness_venue_map(params))),
            ]
        }
        "funding_capture" => {
            let f = FundingCapture::from_params(params);
            vec![
                ("threshold", num(f.threshold)),
                ("qty", num(f.qty)),
                ("symbol", opt_sym(&f.symbol)),
            ]
        }
        "pairs_zscore" => {
            let p = PairsZScore::from_params(params);
            vec![
                ("symbol_a", req_sym(&p.symbol_a)),
                ("symbol_b", req_sym(&p.symbol_b)),
                ("period", p.period.to_string()),
                ("entry_z", num(p.entry_z)),
                ("exit_z", num(p.exit_z)),
                ("beta", num(p.beta)),
                ("notional", num(p.notional)),
                ("taker_fee", num(p.taker_fee)),
                ("half_spread_bps", num(p.half_spread_bps)),
                ("hold_intervals", num(p.hold_intervals)),
                ("funding_a", num(p.funding_a)),
                ("funding_b", num(p.funding_b)),
                ("max_half_life", num(p.max_half_life)),
            ]
        }
        _ => return None,
    })
}

/// Classify `name` for a caller that is about to MOUNT it (as opposed to backtest it). This is the
/// function a daemon's profile validation calls: it distinguishes "typo", "simulator-only" and
/// "resolves but cannot trade live" so the operator gets the right sentence, at profile-LOAD time
/// rather than from a mount that quietly never trades.
pub fn capability(name: &str) -> Capability {
    if let Some((_, why)) = SIMULATOR_ONLY.iter().chain(SCRIPT_ONLY).find(|(n, _)| *n == name) {
        return Capability::SimulatorOnly(why);
    }
    match LIVE_CAPABLE.iter().find(|(n, _)| *n == name) {
        Some((_, None)) => Capability::Live,
        Some((_, Some(why))) => Capability::NotLive(why),
        None => Capability::Unknown,
    }
}

/// A registry lookup failure. Deliberately NOT a `vike-backtest` `HarnessError` — this crate sits
/// below the simulator, and the daemon that also calls this must not have to name one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// `name` is not in [`PORTABLE_STRATEGIES`].
    Unknown(String),
    /// The name resolved but its params are unusable — raised today by the `spread_maker`/
    /// `gueant_maker` arms when a `[strategy.params]` enum key is PRESENT and names no variant
    /// (`vike_mm::ParamError`), so a typo'd `spread_model` fails fast at load rather than silently
    /// backtesting a different model than the profile names. Carries that reader's own message,
    /// which already names the key, the operator's value and the accepted spellings.
    BadParams(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Unknown(name) => {
                write!(f, "unknown strategy {name:?} (known: {})", PORTABLE_STRATEGIES.join(", "))
            }
            RegistryError::BadParams(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Resolve a registry `name` + its TOML `params` table to a boxed strategy, at ANY broker
/// implementing [`HftBroker`] — `vike_backtest::SimBroker` for a backtest,
/// `vike_core::LiveBroker` for a live/paper mount.
///
/// The returned box is `+ Send` because `vike_core::StrategyMount::strategy` is
/// `Box<dyn Strategy<LiveBroker> + Send>` — the live core moves the strategy onto its own thread.
/// A `Box<dyn Strategy<B> + Send>` unsize-coerces to `Box<dyn Strategy<B>>` at a coercion site, so
/// the simulator's own `Box<dyn Strategy<SimBroker>>` callers are unaffected (that is exactly what
/// `vike_backtest::harness::registry::strategy_by_name` does with this result).
///
/// Unknown names are a [`RegistryError::Unknown`] — a profile fails fast, at load time, rather than
/// silently no-opping on a typo'd `strategy.name`.
pub fn strategy_by_name<B: HftBroker + 'static>(
    name: &str,
    params: &Value,
) -> Result<Box<dyn Strategy<B> + Send>, RegistryError> {
    match name {
        "buy_hold" => Ok(Box::new(BuyHold::from_params(params))),
        // The two grid-family reference strategies (`crate::grid_dca`). Both are portable
        // `impl<B: Broker> Strategy<B>` (the `buy_hold` shape) and genuinely param-driven — a sweep
        // grids over `step`/`band`/`size`/`rungs` via `strategy.params` — so they follow the
        // `BuyHold` reader convention rather than `Default::default()`.
        "grid" => Ok(Box::new(Grid::from_params(params))),
        "dca_accumulate" => Ok(Box::new(DcaAccumulate::from_params(params))),
        // The vike-mm Avellaneda–Stoikov market maker. Resolvable at ANY `B: HftBroker`, which is
        // what this whole registry's bound exists for: the same box mounts on `SimBroker`
        // (proven by vike-backtest's `tests/maker_backtest.rs`) and on `LiveBroker` (the mount
        // `vike_run::build_live_maker_core` has always built). UNLIKE every other arm it trades
        // ONLY on a TICK slice: it overrides `on_quote_tick`/`on_order_book` (the L1/L2 maker
        // lanes), NOT `on_bar`, so a bar-only mount never quotes. Param-driven via
        // `SpreadMaker::from_params` (`qty`/`tick_size` + the A-S knobs — see that method's doc).
        // ⚠ That reader now FAILS on a present-but-unrecognized enum value instead of folding it
        // into the default, so a typo'd `spread_model`/`kappa_mode`/`style` stops the profile here
        // rather than backtesting a model nobody selected — hence `BadParams`, not a swallowed
        // fallback. An ABSENT key still keeps its default, so every profile that resolves today
        // still resolves.
        "spread_maker" => Ok(Box::new(spread_maker(params)?)),
        // The GLFT maker: `spread_maker` forced to `SpreadModel::Gueant`. Reads the SAME
        // `[strategy.params]` knobs (gamma/kappa/base_intensity_a/…); a discoverable roster alias
        // for the closed form `spread_maker` also reaches via `spread_model = "gueant"` (#783).
        "gueant_maker" => {
            Ok(Box::new(spread_maker(params)?.with_spread_model(SpreadModel::Gueant)))
        }
        // The Polymarket-native two-buy / delayed-flatten scalper (`crate::trailing_scalper`):
        // rests buy-Up + buy-Down (synthetic), on a fill cancels the opposite and — after
        // `exit_delay_ms` (the reaction gap, default 2s) — flattens the held side. No naked short.
        // Runs on the TICK lane like `spread_maker`; keep the engine order-latency OFF (the delay
        // is in the strategy). Param-driven: `qty`/`half_spread`/`exit_delay_ms`.
        "trailing_scalper" => Ok(Box::new(TrailingScalper::from_params(params))),
        // The two CONTROLLERS, each wrapped in the portable `ControllerHarness` (whose
        // `impl<B: Broker, C: Controller> Strategy<B>` is what turns a controller into a
        // `Strategy<B>`). Both are param-driven like `grid`; the harness-level `venue`/`cooldown_ms`
        // knobs are read by `controller_harness`, the controller-level knobs by each `from_params`.
        "momentum" => {
            Ok(Box::new(controller_harness(MomentumController::from_params(params), params)))
        }
        // Funding-rate CARRY, MULTI-VENUE: a `[strategy.params.venues]` table (`SYMBOL = "venue"`,
        // read by `controller_harness`) routes each series to its OWN venue, so ONE mount fills the
        // cross-venue funding book and opens the carry. `strategy.params.symbol` empty ⇒ the TWO-LEG
        // delta-neutral pair (a leg per carry venue); set ⇒ that symbol's single leg. ⚠ A LIVE mount
        // declares no legs and no live lane carries a funding rate — see [`LIVE_CAPABLE`].
        "funding_carry" => {
            Ok(Box::new(controller_harness(FundingCarryController::from_params(params), params)))
        }
        // The DIRECTIONAL funding-HARVEST reference strategy (`crate::funding_capture`): a
        // single-series `impl<B: Broker>` that reads the funding rate straight off `Bar::funding`
        // and holds the funding-COLLECTING side, sized by `qty` and gated by `threshold`. Needs the
        // backtest's `engine.attach_funding` to feed the funding series onto the replayed bars —
        // which is also why it is a `no` row in [`LIVE_CAPABLE`].
        "funding_capture" => Ok(Box::new(FundingCapture::from_params(params))),
        // The COST-GATED pairs / statistical-arbitrage measurement instrument. TWO-LEG: it needs
        // `symbol_a` + `symbol_b` (no single-symbol fallback exists for a two-leg trade), and a
        // cross-venue or multi-symbol slice to feed both. Its entry requires the expected
        // convergence to EXCEED a full round trip INCLUDING CARRY (`vike_model::spread_total_cost`),
        // so `half_spread_bps`/`taker_fee`/`hold_intervals` are load-bearing knobs.
        "pairs_zscore" => Ok(Box::new(PairsZScore::from_params(params))),
        other => Err(RegistryError::Unknown(other.to_string())),
    }
}

/// The shared body of the `spread_maker` and `gueant_maker` arms: build the maker, and translate
/// `vike_mm`'s param failure into this crate's own error (that message already names the key, the
/// operator's value and the accepted spellings, so nothing is restated here).
fn spread_maker(params: &Value) -> Result<SpreadMaker, RegistryError> {
    SpreadMaker::from_params(params).map_err(|e| RegistryError::BadParams(e.to_string()))
}

/// Read a TOML value as `f64`, accepting either a TOML float or a TOML integer (`size = 1` reads
/// the same as `size = 1.0` — a profile author should not have to remember which).
fn as_f64(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// Wrap a [`Controller`] in its portable [`ControllerHarness`] for the registry, reading the TWO
/// harness-level knobs (NOT controller knobs) from the same params table: `venue` (the single-venue
/// harness's venue tag — default `"sim"`; cosmetic for a `SimBroker` backtest, but the key a
/// `funding_carry` controller reads to pick its leg) and `cooldown_ms` (post-exit re-open spacing —
/// default `0`). The controller's OWN knobs were already read by its `from_params` before it got
/// here.
fn controller_harness<C: Controller>(controller: C, params: &Value) -> ControllerHarness<C> {
    ControllerHarness::new(controller, harness_venue(params), harness_cooldown_ms(params))
        .with_venue_map(harness_venue_map(params))
}

/// The harness's default venue tag. ⚠ The fallback is the literal `"sim"`, which on a LIVE mount is
/// a tag no venue answers to — [`resolved_params`] reports it for exactly that reason.
///
/// Split out of [`controller_harness`] so the mount and the resolved-params ECHO read it ONCE, in
/// one place: two copies of a default is how an echo starts lying about a mount.
fn harness_venue(params: &Value) -> &str {
    params.get("venue").and_then(Value::as_str).unwrap_or("sim")
}

/// Post-exit re-open spacing, default `0`. Same one-read rule as [`harness_venue`].
fn harness_cooldown_ms(params: &Value) -> i64 {
    params.get("cooldown_ms").and_then(Value::as_integer).unwrap_or(0)
}

/// Optional per-symbol venue routing: a `[strategy.params.venues]` table (`SYMBOL = "venue"`) lets a
/// CROSS-VENUE controller (e.g. funding_carry) observe each series under its OWN venue from ONE
/// mount — so its funding book reaches ≥2 venues and the carry can open. Absent ⇒ every symbol
/// routes to the default `venue` and the harness is byte-identical to the single-venue mount.
///
/// ⚠ A row whose VALUE is not a string is dropped silently by the `filter_map` — a per-ROW coercion
/// no key-level type check reaches, which is why the echo prints the surviving map rather than the
/// table that was typed.
fn harness_venue_map(params: &Value) -> Vec<(String, String)> {
    params
        .get("venues")
        .and_then(Value::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(sym, v)| v.as_str().map(|ven| (sym.clone(), ven.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The simplest possible example strategy: buy `size` units of `symbol` ONCE (on the first bar
/// or the first quote tick, whichever the profile's data kind delivers first) and hold — no
/// exit, no re-entry. Exists to smoke-test the registry end-to-end without needing a real
/// strategy; the `docs/superpowers/sdd` tick-profile fixture uses it as `strategy.name =
/// "buy_hold"`.
///
/// PORTABLE (`impl<B: Broker> Strategy<B>`), so it runs unchanged on the live stack. It always
/// called nothing but [`Broker::submit_market`]; what pinned it to the simulator was a private
/// helper typed `&mut SimBroker`, so the widening was a signature change with no call-site rewrite
/// and therefore no behaviour to change. It moved here WITH the registry it is the smoke fixture
/// for; `vike_backtest::harness::BuyHold` still resolves (that module re-exports it).
pub struct BuyHold {
    /// Units to buy (raw, not notional — mirrors [`Broker::submit_market`]'s `qty`).
    pub size: f64,
    /// Explicit symbol override. `None` uses whatever symbol the first bar/tick carries (the
    /// harness's single-symbol path).
    pub symbol: Option<String>,
    bought: bool,
}

impl BuyHold {
    /// `size` defaults to `1.0`; `symbol` is optional (params table: `size = 1.0`, `symbol =
    /// "BTCUSDT"`). Unrecognized/missing keys are simply ignored — this is a params READER, not
    /// a strict schema (the profile's own `deny_unknown_fields` covers the top-level shape).
    pub fn from_params(params: &Value) -> Self {
        let size = params.get("size").and_then(as_f64).unwrap_or(1.0);
        let symbol = params.get("symbol").and_then(Value::as_str).map(str::to_string);
        BuyHold { size, symbol, bought: false }
    }

    /// Construct directly (the test/`Default`-ish path the params reader lowers into).
    pub fn new(size: f64, symbol: Option<String>) -> Self {
        BuyHold { size, symbol, bought: false }
    }

    /// Generic over the broker, which is the ONE thing that made [`BuyHold`] portable: the body is
    /// unchanged and `submit_market` was already the [`Broker`] trait method (`SimBroker` has no
    /// inherent verb of that name), so nothing about what this does moved.
    fn buy<B: Broker>(&mut self, broker: &mut B, symbol: &str) {
        if self.bought || symbol.is_empty() {
            return;
        }
        broker.submit_market(symbol, 1, self.size);
        self.bought = true;
    }
}

impl<B: Broker> Strategy<B> for BuyHold {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol = self.symbol.clone().or_else(|| bar.symbol.clone()).unwrap_or_default();
        self.buy(broker, &symbol);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        let symbol = self.symbol.clone().unwrap_or_else(|| q.symbol.clone());
        self.buy(broker, &symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal concrete [`HftBroker`] to instantiate the generic resolver at. `vike_model`'s own
    /// `MockBroker` implements [`Broker`] but NOT [`HftBroker`] (the maker arms need the tagged
    /// verbs), and the two REAL brokers — `SimBroker` / `LiveBroker` — both live ABOVE this crate,
    /// so a local double is the only way to run these here. Instantiating at ONE broker is enough:
    /// a generic function's body is type-checked at its definition, so the arms are proven for
    /// every `B: HftBroker`.
    ///
    /// It COUNTS submits (and nothing else): `the_empty_symbol_this_echo_reports_really_does_stop_
    /// the_strategy` needs to tell "routed an order" from "routed nothing", which a broker that
    /// swallows every call cannot answer. Counting rather than recording keeps the double a probe.
    #[derive(Default)]
    struct ProbeBroker {
        submits: usize,
    }

    impl Broker for ProbeBroker {
        fn submit_market(&mut self, _symbol: &str, _side: i32, _qty: f64) {
            self.submits += 1;
        }
        fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {
            self.submits += 1;
        }
        fn position(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn price(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _symbol: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }

    impl HftBroker for ProbeBroker {
        fn position(&self) -> f64 {
            0.0
        }
        fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {
            self.submits += 1;
        }
        fn modify_tagged(&mut self, _tag: &str, _q: Option<f64>, _p: Option<f64>) {}
        fn cancel_tagged(&mut self, _tag: &str) {}
    }

    /// [`ProbeBroker`]'s RECORDING sibling — it keeps the `(symbol, side, qty)` of every submission
    /// instead of counting them, because the two route tests below are about WHAT reached the broker
    /// and not how often. Kept separate rather than widening `ProbeBroker`: that one is deliberately
    /// a counter (its own doc says so), and the tests that use it read `submits`.
    #[derive(Default)]
    struct RecordingBroker {
        submitted: Vec<(String, i32, f64)>,
        price: f64,
    }

    impl Broker for RecordingBroker {
        fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
            self.submitted.push((symbol.to_string(), side, qty));
        }
        fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, _price: f64) {
            self.submitted.push((symbol.to_string(), side, qty));
        }
        fn position(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn price(&self, _symbol: &str) -> f64 {
            self.price
        }
        fn equity(&self) -> f64 {
            1_000.0
        }
        fn bars(&self, _symbol: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }

    impl HftBroker for RecordingBroker {
        fn position(&self) -> f64 {
            0.0
        }
        fn submit_limit_tagged(&mut self, _tag: &str, side: i32, qty: f64, _price: f64) {
            self.submitted.push((String::new(), side, qty));
        }
        fn modify_tagged(&mut self, _tag: &str, _q: Option<f64>, _p: Option<f64>) {}
        fn cancel_tagged(&mut self, _tag: &str) {}
    }

    /// A bar the RUNTIME would deliver: symbol-stamped with the MOUNT's own instrument
    /// (`crates/vike-core/src/runtime/mod.rs`'s `BarClose` arm sets `bar.symbol = Some(key.1)`).
    fn mounted_bar(ts: i64, close: f64) -> Bar {
        Bar {
            ts,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("MOUNTED".to_string()),
        }
    }

    fn resolve(
        name: &str,
        params: &Value,
    ) -> Result<Box<dyn Strategy<ProbeBroker> + Send>, RegistryError> {
        strategy_by_name::<ProbeBroker>(name, params)
    }

    fn empty() -> Value {
        Value::Table(Default::default())
    }

    #[test]
    fn registry_lists_every_match_arm() {
        // Every name PORTABLE_STRATEGIES advertises must actually resolve with DEFAULT params —
        // keeps the const in sync with the match by construction, not by convention.
        for name in PORTABLE_STRATEGIES {
            assert!(resolve(name, &empty()).is_ok(), "{name} should resolve");
        }
    }

    #[test]
    fn unknown_name_is_a_registry_error_naming_the_roster() {
        match resolve("nope", &empty()) {
            Err(e @ RegistryError::Unknown(_)) => {
                let msg = e.to_string();
                assert!(msg.contains("nope"), "names the typo: {msg}");
                assert!(msg.contains("buy_hold"), "names the roster: {msg}");
            }
            other => panic!("expected Unknown, got Ok={}", other.is_ok()),
        }
    }

    /// The registry's whole reason for existing: ONE resolver, TWO brokers. A generic function's
    /// body is type-checked at its DEFINITION, so this compiling IS the proof that every arm holds
    /// for every `B: HftBroker` — including `vike_core::LiveBroker`, which this crate cannot name.
    #[test]
    fn the_resolver_is_generic_over_every_hft_broker() {
        fn probe<B: HftBroker + 'static>() {
            let _ = strategy_by_name::<B>("buy_hold", &Value::Table(Default::default()));
        }
        probe::<ProbeBroker>();
    }

    /// The returned box must satisfy `vike_core::StrategyMount::strategy`'s `+ Send` bound — the
    /// live core moves the strategy onto its own thread. Asserted here rather than trusted: a
    /// future arm holding an `Rc` would compile everywhere else and fail only at the live mount.
    #[test]
    fn every_resolved_strategy_is_send() {
        fn assert_send<T: Send>(_t: &T) {}
        for name in PORTABLE_STRATEGIES {
            let s = resolve(name, &empty()).expect("resolves");
            assert_send(&s);
        }
    }

    #[test]
    fn live_capable_table_is_exhaustive() {
        // Every roster name has exactly one verdict row, and no row names a strategy that is not on
        // the roster. Adding an arm without classifying it fails HERE, which is the point: an
        // unclassified strategy is one nobody decided could trade.
        for name in PORTABLE_STRATEGIES {
            assert_eq!(
                LIVE_CAPABLE.iter().filter(|(n, _)| n == name).count(),
                1,
                "{name} needs exactly one LIVE_CAPABLE row"
            );
        }
        for (name, _) in LIVE_CAPABLE {
            assert!(
                PORTABLE_STRATEGIES.contains(name),
                "LIVE_CAPABLE names {name}, which is not on the roster"
            );
        }
    }

    #[test]
    fn every_not_live_row_carries_a_nonempty_reason() {
        // A `no` with no reason is an opinion; a `no` with a named missing input is a claim
        // somebody can check and later disprove.
        for (name, why) in LIVE_CAPABLE {
            if let Some(reason) = why {
                assert!(reason.len() > 20, "{name}'s reason is too thin to act on: {reason:?}");
            }
        }
    }

    #[test]
    fn simulator_only_and_portable_rosters_are_disjoint() {
        for (name, _) in SIMULATOR_ONLY {
            assert!(
                !PORTABLE_STRATEGIES.contains(name),
                "{name} is claimed by BOTH rosters — one of them is wrong"
            );
        }
    }

    #[test]
    fn capability_distinguishes_the_four_answers() {
        assert_eq!(capability("spread_maker"), Capability::Live);
        assert!(matches!(capability("funding_capture"), Capability::NotLive(_)));
        assert!(matches!(capability("rotation_top_k"), Capability::SimulatorOnly(_)));
        assert_eq!(capability("nope"), Capability::Unknown);
    }

    /// The SCRIPT path's NAME is a real registry arm this crate cannot resolve — not a typo.
    /// Before [`SCRIPT_ONLY`] existed, a profile naming it was told it did not exist, which is the
    /// one answer that is simply false and sends an operator hunting for a misspelling. Since the
    /// 0024 reversal the reason must ALSO point at the spelling that DOES mount a script live —
    /// the daemon's `rhai = "<path>"` — because "resolves only in the backtest" without that
    /// pointer reads as the pre-reversal "scripts cannot go live", which is no longer true.
    ///
    /// ⚠ It must NOT cite the record itself, and this test asserted the opposite until 2026-09-04.
    /// This string is EXPORTED — it reaches `templates.json` and renders onto
    /// `vike.io/docs/trader/strategies/simulator-only`, whose reader's only view of this workspace
    /// is the public source mirror, and the mirror publishes no `docs/` at all. So the citation was
    /// a dead link on a public page, sitting in the one sentence a reader would want to follow.
    /// The INFORMATION the record carries survives in the `rhai = "<path>"` pointer above, which is
    /// the actionable half; the reasoning stays in this module's doc comments, which are not
    /// published as documentation. `crates/vike-ops/tests/docs_data_gate.rs`'s
    /// `no_rendered_asset_cites_a_path_the_mirror_withholds` is the gate for the whole class.
    #[test]
    fn the_script_strategy_is_named_rather_than_called_a_typo() {
        match capability("rhai") {
            Capability::SimulatorOnly(why) => {
                assert!(why.contains("src"), "names the param the NAME arm needs: {why}");
                assert!(why.contains("vike-script"), "names what does not link here: {why}");
                assert!(
                    why.contains("rhai = "),
                    "points at the daemon's live path spelling: {why}"
                );
                assert!(
                    !why.contains("docs/"),
                    "an EXPORTED string may not cite a path the public mirror withholds: {why}"
                );
            }
            other => panic!("rhai must not read as {other:?}"),
        }
    }

    #[test]
    fn script_only_is_disjoint_from_both_rosters() {
        for (name, why) in SCRIPT_ONLY {
            assert!(!PORTABLE_STRATEGIES.contains(name), "{name} is on the portable roster");
            assert!(
                !SIMULATOR_ONLY.iter().any(|(n, _)| n == name),
                "{name} is claimed by SIMULATOR_ONLY too — one of the two rows is wrong"
            );
            assert!(why.len() > 20, "{name}'s reason is too thin to act on: {why:?}");
        }
    }

    /// The `NotLive` rows are the ones that would SILENTLY never trade (or, for
    /// `trailing_scalper`, trade HALF of what was backtested). Pin them by name: a future PR that
    /// wires the missing input flips the row deliberately and updates this list, rather than a
    /// rename quietly making a footgun mountable.
    #[test]
    fn the_not_live_set_is_exactly_the_known_gaps() {
        let not_live: Vec<&str> =
            LIVE_CAPABLE.iter().filter(|(_, w)| w.is_some()).map(|(n, _)| *n).collect();
        assert_eq!(
            not_live,
            vec!["trailing_scalper", "funding_carry", "funding_capture", "pairs_zscore"]
        );
    }

    #[test]
    fn param_keys_table_is_exhaustive() {
        // Same construction as `live_capable_table_is_exhaustive`: adding a registry arm without
        // declaring what its params table may contain fails HERE, because an undeclared reader is
        // one whose typos a live mount cannot catch.
        for name in PORTABLE_STRATEGIES {
            assert_eq!(
                PARAM_KEYS.iter().filter(|(n, _)| n == name).count(),
                1,
                "{name} needs exactly one PARAM_KEYS row"
            );
        }
        for (name, keys) in PARAM_KEYS {
            assert!(
                PORTABLE_STRATEGIES.contains(name),
                "PARAM_KEYS names {name}, which is not on the roster"
            );
            match keys {
                ParamKeys::Declared(k) => {
                    assert!(!k.is_empty(), "{name} declares an EMPTY key set — say NotEnumerated");
                    let mut sorted: Vec<&str> = k.iter().map(|(n, _)| *n).collect();
                    sorted.sort_unstable();
                    sorted.dedup();
                    assert_eq!(sorted.len(), k.len(), "{name} declares a key twice");
                }
                ParamKeys::NotEnumerated(why) => {
                    assert!(why.len() > 20, "{name}'s NotEnumerated reason is too thin: {why:?}")
                }
            }
        }
    }

    /// A declared key whose NAME is route-shaped, in the vocabulary this workspace's params readers
    /// actually use. The reverse-direction gate below forces a [`PARAM_ROUTES`] row for every one of
    /// them, so a new strategy declaring `symbol` cannot inherit the inert-knob trap in silence.
    ///
    /// ⚠ A NAME heuristic, deliberately, and it is a LOWER bound — a route key called `market` or
    /// `instrument` would pass unseen. That is honest rather than tidy: the alternative is resolving
    /// what a reader STORES a key into, which means guessing at bodies the
    /// `crates/vike-strategy/tests/param_keys_gate.rs` scanner already declines to slice. The
    /// heuristic can only ever over-fire (demanding a row for a key that turns out not to route),
    /// and an over-fire is one written row, not a silent hole. `the_route_shape_gate_can_fail` is
    /// its mutation self-test.
    fn is_route_shaped(key: &str) -> bool {
        key == "symbol"
            || key.starts_with("symbol_")
            || key == "venue"
            || key == "venues"
            || key.starts_with("venue_")
    }

    #[test]
    fn param_routes_table_is_exhaustive() {
        // Same construction as `param_keys_table_is_exhaustive` and for the harder reason: an
        // unclassified name is one whose route keys nothing refuses, and a route key nothing
        // refuses names an instrument real orders do not go to.
        for name in PORTABLE_STRATEGIES {
            assert_eq!(
                PARAM_ROUTES.iter().filter(|(n, _)| n == name).count(),
                1,
                "{name} needs exactly one PARAM_ROUTES row"
            );
        }
        for (name, routes) in PARAM_ROUTES {
            assert!(
                PORTABLE_STRATEGIES.contains(name),
                "PARAM_ROUTES names {name}, which is not on the roster"
            );
            match routes {
                ParamRoutes::SingleLeg(_) => {}
                ParamRoutes::MultiLeg(keys, why) => {
                    assert!(
                        !keys.is_empty(),
                        "{name} is MultiLeg with no route key — say SingleLeg(&[])"
                    );
                    assert!(
                        why.len() > 20,
                        "{name}'s MultiLeg reason is too thin to act on: {why:?}"
                    );
                }
                ParamRoutes::NotEnumerated(why) => assert!(
                    why.len() > 20,
                    "{name}'s NotEnumerated reason is too thin to act on: {why:?}"
                ),
            }
        }
    }

    /// Direction 1: a route row may only name keys that name something.
    ///
    /// The key must be a DECLARED [`PARAM_KEYS`] key of the same name (a route row over a key no
    /// reader reads would refuse a profile for a knob that never existed), and its declared
    /// [`ParamType`] must match what [`misrouted_params`] reads it through — `Str` for a
    /// `Symbol`/`Venue`, `Table` for a `VenueMap`. That second half is what stops the two tables
    /// drifting into a check that silently never fires: `misrouted_params` reads a `Symbol` key
    /// with `as_str`, so a key declared `Number` would match nothing, forever, green.
    #[test]
    fn every_route_key_is_a_declared_key_of_the_right_type() {
        for (name, routes) in PARAM_ROUTES {
            let keys: &[(&str, RouteKind)] = match routes {
                ParamRoutes::SingleLeg(k) | ParamRoutes::MultiLeg(k, _) => k,
                ParamRoutes::NotEnumerated(_) => {
                    // Mirrors its PARAM_KEYS row, and must: a name whose key set is unknown cannot
                    // have a known route subset.
                    assert!(
                        matches!(param_keys(name), Some(ParamKeys::NotEnumerated(_))),
                        "{name}'s PARAM_ROUTES row is NotEnumerated but its PARAM_KEYS row is not"
                    );
                    continue;
                }
            };
            let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
                panic!("{name} declares route keys but enumerates no params keys");
            };
            for (key, kind) in keys {
                let Some((_, ty)) = declared.iter().find(|(n, _)| n == key) else {
                    panic!("{name}'s route key `{key}` is not a declared PARAM_KEYS key");
                };
                let want = match kind {
                    RouteKind::Symbol | RouteKind::Venue => ParamType::Str,
                    RouteKind::VenueMap => ParamType::Table,
                };
                assert_eq!(
                    *ty, want,
                    "{name}'s route key `{key}` is {kind:?}, which `misrouted_params` reads as \
                     {want:?} — but PARAM_KEYS declares it {ty:?}, so the check would never fire"
                );
            }
        }
    }

    /// Direction 2: a route-shaped declared key may not go unclassified.
    ///
    /// This is the direction that matters for a FUTURE strategy. `buy_hold`'s `symbol` was read,
    /// well-typed and echoed for three review rounds while the mount overrode it; the only thing
    /// that stops the fourth instance is a gate that fires when somebody declares the next one.
    #[test]
    fn every_route_shaped_declared_key_has_a_route_row() {
        for (name, keys) in PARAM_KEYS {
            let ParamKeys::Declared(declared) = keys else {
                continue;
            };
            let routed: Vec<&str> = match param_routes(name) {
                Some(ParamRoutes::SingleLeg(k)) | Some(ParamRoutes::MultiLeg(k, _)) => {
                    k.iter().map(|(n, _)| *n).collect()
                }
                _ => Vec::new(),
            };
            for (key, _) in declared.iter() {
                if is_route_shaped(key) {
                    assert!(
                        routed.contains(key),
                        "{name} declares the route-shaped key `{key}` with no PARAM_ROUTES row: a \
                         mount would OVERRIDE it (`resolve_intent_symbol`/`resolve_intent_venue`) \
                         and nothing would refuse the profile that set it"
                    );
                }
            }
        }
    }

    /// The mutation self-test for the shape heuristic above — a gate nobody proved can fail is how
    /// this defect class survived two rounds of review.
    #[test]
    fn the_route_shape_gate_can_fail() {
        assert!(is_route_shaped("symbol"), "the real key that carried the defect");
        assert!(is_route_shaped("symbol_a") && is_route_shaped("symbol_b"), "the two-leg spelling");
        assert!(is_route_shaped("venue") && is_route_shaped("venues"), "the venue half");
        // …and it must NOT swallow the knob keys, or direction 2 would demand a route row for every
        // key in the table and the distinction would carry no information.
        for knob in ["size", "qty", "step", "rungs", "tp", "sl", "cooldown_ms", "anchor_price"] {
            assert!(!is_route_shaped(knob), "`{knob}` is a knob, not a route");
        }
        // The gate's own input must be non-empty: if no declared key were route-shaped, direction 2
        // would pass vacuously forever.
        let route_shaped = PARAM_KEYS
            .iter()
            .filter_map(|(_, k)| match k {
                ParamKeys::Declared(d) => Some(d),
                ParamKeys::NotEnumerated(_) => None,
            })
            .flat_map(|d| d.iter())
            .filter(|(key, _)| is_route_shaped(key))
            .count();
        assert!(route_shaped > 0, "no declared key is route-shaped — direction 2 is vacuous");
    }

    /// `misrouted_params` reports exactly the values a mount would OVERRIDE, and nothing else.
    ///
    /// Each assertion is a distinct arm of the rule rather than a restatement of it: the three
    /// symbol states (absent / empty / naming another instrument), the venue half, the routing
    /// table's per-row rule, and the two abstentions (multi-leg names, and a wrong TYPE, which is
    /// `mistyped_params`' finding).
    #[test]
    fn misrouted_params_reports_only_what_the_mount_would_override() {
        let p = |src: &str| toml::from_str::<Value>(src).expect("test params parse");
        let keys = |src: &str| -> Vec<String> {
            misrouted_params("buy_hold", &p(src), "polymarket", "MOUNTED")
                .into_iter()
                .map(|m| m.key)
                .collect()
        };
        // ABSENT: the working default — the runtime stamps the mount's symbol onto the bar.
        assert!(keys("size = 1.0").is_empty());
        // EMPTY: a mount that cannot trade, which `resolved_params`' `opt_sym` already reports.
        assert!(keys("symbol = \"\"").is_empty());
        // AGREES: a no-op restatement of the mount, and legal.
        assert!(keys("symbol = \"MOUNTED\"").is_empty());
        // DISAGREES: the defect. One finding, naming BOTH instruments.
        let bad = misrouted_params("buy_hold", &p("symbol = \"OTHER\""), "polymarket", "MOUNTED");
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].key, "symbol");
        let said = bad[0].to_string();
        assert!(said.contains("OTHER") && said.contains("MOUNTED"), "names both: {said}");
        // A wrong TYPE is `mistyped_params`' finding — reporting it here too would hand the
        // operator two sentences about one slip, the second of them confusing.
        assert!(
            misrouted_params("buy_hold", &p("symbol = 7"), "polymarket", "MOUNTED").is_empty(),
            "a non-string symbol belongs to the TYPE check, not the ROUTE check"
        );

        // The VENUE half, on the one Live name that has one.
        let venue_keys = |src: &str| -> Vec<String> {
            misrouted_params("momentum", &p(src), "polymarket", "MOUNTED")
                .into_iter()
                .map(|m| m.key)
                .collect()
        };
        assert!(venue_keys("qty = 1.0").is_empty(), "an absent venue leaves the harness default");
        assert!(venue_keys("venue = \"polymarket\"").is_empty(), "agreeing is legal");
        assert_eq!(venue_keys("venue = \"binance\""), vec!["venue".to_string()]);
        // The routing TABLE, row by row: only this mount's own route is legal.
        assert!(venue_keys("[venues]\nMOUNTED = \"polymarket\"\n").is_empty());
        assert_eq!(
            venue_keys("[venues]\nMOUNTED = \"binance\"\n"),
            vec!["venues.MOUNTED".to_string()],
            "a row naming another VENUE is discarded by `resolve_intent_venue`"
        );
        assert_eq!(
            venue_keys("[venues]\nOTHER = \"polymarket\"\n"),
            vec!["venues.OTHER".to_string()],
            "a row naming another SYMBOL never matches the one series this mount receives"
        );
        assert_eq!(
            venue_keys("[venues]\nMOUNTED = 7\n"),
            vec!["venues.MOUNTED".to_string()],
            "a non-string row is dropped by `harness_venue_map`'s filter_map"
        );

        // MULTI-LEG names abstain: `symbol_a`/`symbol_b` name legs, not this mount's market.
        assert!(
            misrouted_params("pairs_zscore", &p("symbol_a = \"A\"\nsymbol_b = \"B\""), "v", "M")
                .is_empty(),
            "a two-leg name's keys are not claims about a single-leg mount"
        );
        // …and so does an unknown name, and a `NotEnumerated` one.
        assert!(misrouted_params("nope", &p("symbol = \"OTHER\""), "v", "M").is_empty());
        assert!(misrouted_params("spread_maker", &p("symbol = \"OTHER\""), "v", "M").is_empty());
    }

    /// The CLAIM behind the refusal, proven rather than asserted: with a params `symbol` that
    /// disagrees with the dispatching series, the strategy really does submit under the name it was
    /// handed — which is what the mount then overrides. If `BuyHold` ever started ignoring its own
    /// `symbol` field, the refusal would be guarding nothing and this goes red.
    #[test]
    fn a_disagreeing_params_symbol_really_is_what_the_strategy_submits() {
        let params: Value = toml::from_str("size = 3.0\nsymbol = \"OTHER\"\n").unwrap();
        let mut s = BuyHold::from_params(&params);
        let mut broker = RecordingBroker::default();
        // The bar the RUNTIME delivers carries the MOUNT's symbol; the params key overrides it here,
        // and the mount then overrides it back — which is the whole defect.
        s.on_bar(&mut broker, &mounted_bar(1, 1.0));
        assert_eq!(
            broker.submitted.iter().map(|(s, _, _)| s.as_str()).collect::<Vec<_>>(),
            vec!["OTHER"],
            "the params symbol must be what reaches the broker — otherwise the mount's override \
             (and the refusal that now prevents it) would be guarding nothing"
        );
    }

    /// The DECLARED RESIDUAL on the `momentum` route row, proven rather than asserted: the harness's
    /// `venue` is a LABEL, not an order destination.
    ///
    /// This is what licenses [`resolved_params`] to keep echoing `venue=sim` for a mount that has no
    /// `venue` key (`the_echo_reports_the_resolution_and_not_the_input` in
    /// `crates/vike-tradehub/src/config.rs` pins that deliberately) instead of that being a fourth
    /// instance of the "a diagnostic claims something false" class. Two harnesses that differ ONLY in
    /// their venue tag, driven over identical bars, must produce IDENTICAL submissions — because
    /// [`vike_model::Broker`]'s submit verbs take no venue at all, so the tag has nowhere to go.
    ///
    /// MUTATION: make the two params tables agree (`venue = "one"` on both) and the equality below
    /// holds for the trivial reason instead of the real one — hence the non-vacuity assert.
    #[test]
    fn the_harness_venue_is_a_label_and_not_an_order_destination() {
        let drive = |venue: &str| -> Vec<(String, i32, f64)> {
            let params: Value =
                toml::from_str(&format!("qty = 2.0\nvenue = \"{venue}\"\n")).unwrap();
            // NOT the `resolve` helper above — that one is pinned to `ProbeBroker`, which counts
            // rather than records. Same registry call, instantiated at the recording double.
            let mut s = strategy_by_name::<RecordingBroker>("momentum", &params)
                .expect("momentum resolves");
            let mut broker = RecordingBroker::default();
            // Two bars with a rising price: the controller declines its first invitation (no
            // reference yet) and opens on the second, at the default `threshold = 0`.
            for (i, px) in [1.0_f64, 2.0].into_iter().enumerate() {
                broker.price = px;
                s.on_bar(&mut broker, &mounted_bar(60_000 * (i as i64 + 1), px));
            }
            broker.submitted
        };
        let one = drive("A_VENUE");
        let other = drive("ANOTHER_VENUE");
        assert!(
            !one.is_empty(),
            "the harness submitted nothing, so the equality below would hold vacuously"
        );
        assert_eq!(
            one, other,
            "two harnesses differing ONLY in their venue tag produced DIFFERENT orders — the tag \
             would then be an order destination and echoing `venue=sim` really would be a false \
             claim about where orders go"
        );
        // ...and the orders carry the MOUNT's symbol either way — the harness routes off the bar.
        assert!(one.iter().all(|(sym, _, _)| sym == "MOUNTED"), "{one:?}");
    }

    /// [`PARAM_GATES`]' structural direction: a row may only name keys that exist, on a name that
    /// exists, once. Same construction as `every_route_key_is_a_declared_key_of_the_right_type` and
    /// for the same reason — a gate over a key no reader reads would exempt a knob that never
    /// existed from the class-closer, and a gate READING a key [`resolved_params`] does not carry
    /// would render `(absent)` forever, exempting its key unconditionally.
    #[test]
    fn every_gate_names_a_declared_key_of_the_same_strategy() {
        for (name, key, gate) in PARAM_GATES {
            let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
                panic!("PARAM_GATES names {name}, which declares no params keys")
            };
            assert!(
                declared.iter().any(|(n, _)| n == key),
                "{name}'s gate is over `{key}`, which is not a declared PARAM_KEYS key"
            );
            for read in gate.keys() {
                assert!(
                    declared.iter().any(|(n, _)| *n == read),
                    "{name}'s gate on `{key}` reads `{read}`, which {name} does not declare — \
                     `resolved_params` would never carry it and the gate would read `(absent)` \
                     forever"
                );
                assert_ne!(
                    read, *key,
                    "{name}'s gate on `{key}` reads `{key}` — a key whose OWN value picks a branch \
                     is CONSUMED in both branches, which is the opposite of this table's claim"
                );
            }
            assert_eq!(
                PARAM_GATES.iter().filter(|(n, k, _)| n == name && k == key).count(),
                1,
                "{name}'s `{key}` has more than one gate row — say All(&[..])"
            );
        }
    }

    /// The gate's own mutation self-test — [`Gate::unmet`] is pure, so prove it says NOTHING for a
    /// met gate and names the OFFENDING key for an unmet one, in every variant. Without this the
    /// class-closer's exemptions could be vacuously green: a gate that never reports an unmet
    /// conjunct exempts its key from direction 2 while direction 1 has nothing to measure.
    #[test]
    fn the_gate_predicate_can_actually_fail() {
        let rows = |pairs: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
            pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
        };
        let fixed = rows(&[("anchor", "fixed")]);
        let first = rows(&[("anchor", "first")]);
        assert!(Gate::Is("anchor", &["fixed"]).unmet(&fixed).is_empty());
        assert_eq!(Gate::Is("anchor", &["fixed"]).unmet(&first), vec!["anchor=first".to_string()]);
        // Positive is a NUMERIC test, so `0` and a negative both close it and a non-number does too.
        assert!(Gate::Positive("rungs").unmet(&rows(&[("rungs", "3")])).is_empty());
        assert_eq!(Gate::Positive("rungs").unmet(&rows(&[("rungs", "0")])), vec!["rungs=0"]);
        assert_eq!(Gate::Positive("size").unmet(&rows(&[("size", "-2")])), vec!["size=-2"]);
        assert_eq!(Gate::Positive("size").unmet(&rows(&[("size", "n/a")])), vec!["size=n/a"]);
        // `All` reports EVERY failing conjunct, so the operator sees all the reasons at once.
        let both = Gate::All(&[Gate::Positive("rungs"), Gate::Positive("size")]);
        assert!(both.unmet(&rows(&[("rungs", "3"), ("size", "1")])).is_empty());
        assert_eq!(
            both.unmet(&rows(&[("rungs", "0"), ("size", "0")])),
            vec!["rungs=0".to_string(), "size=0".to_string()]
        );
        // A key the echo does not carry is LOUD rather than silently "consumed".
        assert_eq!(Gate::Positive("nope").unmet(&fixed), vec!["nope=(absent)".to_string()]);
        // ...and `keys` reaches through `All`, which is what the structural gate walks.
        assert_eq!(both.keys(), vec!["rungs", "size"]);
    }

    #[test]
    fn unknown_params_names_the_typo_and_passes_the_real_key() {
        let params: Value = toml::from_str("size = 2.0\nsizee = 3.0\nzzz = 1\n").unwrap();
        assert_eq!(
            unknown_params("buy_hold", &params),
            vec!["sizee".to_string(), "zzz".to_string()]
        );
        // A fully-recognised table is clean...
        let ok: Value = toml::from_str("size = 2.0\nsymbol = \"BTC\"\n").unwrap();
        assert!(unknown_params("buy_hold", &ok).is_empty());
        // ...a NotEnumerated row reports nothing (its consumer owes the stricter rule)...
        assert!(unknown_params("spread_maker", &params).is_empty());
        // ...and so does a name this registry does not resolve.
        assert!(unknown_params("nope", &params).is_empty());
    }

    /// A declared key carrying a value of the WRONG TYPE is reported, naming both types. This is
    /// blocker 2's remaining half: `size = "2"` is a key the reader knows and a value it cannot
    /// take, so `and_then(as_f64)` yields `None` and the knob mounts at its compiled default with
    /// the profile stating otherwise.
    #[test]
    fn mistyped_params_names_the_key_and_both_types() {
        // The reviewer's own four examples, verbatim.
        let quoted: Value = toml::from_str("size = \"2\"\n").unwrap();
        let e = mistyped_params("grid", &quoted);
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].key, "size");
        assert_eq!(e[0].got, "string");
        assert!(e[0].expected.contains("number"), "{:?}", e[0]);
        // ...and it really would have mounted the compiled default.
        assert_eq!(Grid::from_params(&quoted).size, Grid::default().size);

        let float_count: Value = toml::from_str("rungs = 4.0\n").unwrap();
        let e = mistyped_params("grid", &float_count);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].key.as_str(), e[0].got, e[0].expected), ("rungs", "float", "an integer"));
        assert_eq!(Grid::from_params(&float_count).rungs, Grid::default().rungs);

        let boolean: Value = toml::from_str("band = true\n").unwrap();
        let e = mistyped_params("grid", &boolean);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].key.as_str(), e[0].got), ("band", "boolean"));
        assert_eq!(Grid::from_params(&boolean).band, Grid::default().band);

        let e = mistyped_params("buy_hold", &toml::from_str("size = \"3\"\n").unwrap());
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].key.as_str(), e[0].got), ("size", "string"));
    }

    /// The abstentions, each for its own reason — the same three [`unknown_params`] has, plus the
    /// one that matters most: a key at a type the reader DOES take is not an error, because a rule
    /// refusing `qty = 1` where the reader happily takes `1.0` would break working profiles.
    #[test]
    fn mistyped_params_accepts_every_spelling_its_reader_accepts() {
        // The lenient numeric convention: BOTH spellings are legal for an `as_f64` key.
        for src in ["size = 2", "size = 2.0"] {
            assert!(
                mistyped_params("grid", &toml::from_str(src).unwrap()).is_empty(),
                "`{src}` must be accepted — `as_f64` takes either"
            );
        }
        // `read_side` genuinely takes EITHER, so both must pass.
        for src in ["side = \"short\"", "side = -1"] {
            assert!(
                mistyped_params("dca_accumulate", &toml::from_str(src).unwrap()).is_empty(),
                "`{src}` must be accepted — `read_side` takes either"
            );
        }
        // ...and a third type on that same key is still refused.
        assert_eq!(
            mistyped_params("dca_accumulate", &toml::from_str("side = 1.0").unwrap()).len(),
            1,
            "a float `side` reads as nothing in either arm of `read_side`"
        );
        // An UNKNOWN key is `unknown_params`' business, not this one's.
        assert!(mistyped_params("grid", &toml::from_str("sizee = \"2\"").unwrap()).is_empty());
        // A NotEnumerated row, an unknown name and a non-table all abstain.
        let bad: Value = toml::from_str("qty = \"2\"").unwrap();
        assert!(mistyped_params("spread_maker", &bad).is_empty());
        assert!(mistyped_params("nope", &bad).is_empty());
        assert!(mistyped_params("grid", &Value::Integer(3)).is_empty());
    }

    /// The FOURTH reader, and the case the three above all pass: every key spelled right, typed
    /// right and routed right, and the ladder they describe between them is EMPTY.
    ///
    /// The two rows that motivated it are the measured ones from
    /// `crates/vike-strategy/tests/param_gates.rs`'s `DEAD` ledger. The rows BELOW them matter as
    /// much: this must be a rule about an empty ladder and never about a suspicious VALUE, or it
    /// becomes the over-refusal it exists to avoid.
    #[test]
    fn unarmable_params_names_the_empty_ladder_and_its_resolution() {
        let at = |name: &str, src: &str| {
            unarmable_params(name, &toml::from_str::<Value>(src).expect("test TOML"))
        };
        // A FIXED anchor left at its compiled default price.
        let why = at("dca_accumulate", "anchor = \"fixed\"").expect("refused");
        assert!(why.contains("dca_accumulate"), "{why}");
        assert!(why.contains("NO rung"), "names what is wrong: {why}");
        assert!(why.contains("anchor=fixed"), "carries the resolution: {why}");
        assert!(why.contains("anchor_price=0"), "...including the knob left at its default: {why}");
        // A 0..1 grid at the compiled `step = 1.0`.
        let why = at("grid", "bounded01 = true").expect("refused");
        assert!(why.contains("bounded01=true") && why.contains("step=1"), "{why}");
        // ...and the degenerate ladder, which is the same defect: no rungs, no size, no spacing.
        for src in ["rungs = 0", "rungs = -5", "size = 0.0", "step = 0.0"] {
            assert!(at("grid", src).is_some(), "grid `{src}`");
            assert!(at("dca_accumulate", src).is_some(), "dca `{src}`");
        }
        // ⚠ The near-misses, which a rule about VALUES rather than ladders would wrongly refuse: a
        // SHORT ladder anchored at zero rests `step`, `2·step`, … and a bounded grid whose step
        // fits inside the 0..1 walls rests rungs.
        assert!(
            at("dca_accumulate", "anchor = \"fixed\"\nside = \"short\"\nstep = 0.05").is_none()
        );
        assert!(at("grid", "bounded01 = true\nstep = 0.05").is_none());
        // The ordinary tables both names ship with are armable, or every row above is trivial.
        for name in ["grid", "dca_accumulate"] {
            assert!(at(name, "").is_none(), "{name}'s own defaults must load");
            assert!(at(name, "rungs = 4\nstep = 0.5\nsize = 2.0").is_none(), "{name}");
        }
        // Every other name abstains — the question is not asked of a strategy whose order flow is a
        // function of the market rather than of the table.
        for name in ["buy_hold", "momentum", "trailing_scalper", "spread_maker", "nope"] {
            assert!(unarmable_params(name, &empty()).is_none(), "{name} must abstain");
        }
    }

    /// The echo's key set is the TABLE's key set, in order — so a knob added to a reader (and
    /// therefore to [`PARAM_KEYS`], which its own gate enforces) cannot be left out of the mount log.
    #[test]
    fn resolved_params_reports_exactly_the_declared_keys_in_order() {
        for (name, keys) in PARAM_KEYS {
            match keys {
                ParamKeys::Declared(declared) => {
                    let got = resolved_params(name, &empty())
                        .unwrap_or_else(|| panic!("{name} declares keys but reports none"));
                    let got_keys: Vec<&str> = got.iter().map(|(k, _)| *k).collect();
                    let want: Vec<&str> = declared.iter().map(|(k, _)| *k).collect();
                    assert_eq!(got_keys, want, "{name}'s echo and its PARAM_KEYS row disagree");
                    assert!(
                        got.iter().all(|(_, v)| !v.is_empty()),
                        "{name} echoes an EMPTY value — say what it resolved to"
                    );
                }
                ParamKeys::NotEnumerated(_) => assert!(
                    resolved_params(name, &empty()).is_none(),
                    "{name} enumerates nothing, so it can report nothing"
                ),
            }
        }
        assert!(resolved_params("nope", &empty()).is_none());
    }

    /// The echo reports the RESOLVED value, not the typed one — which is the whole point, because
    /// the divergences a type check cannot catch are exactly the ones nobody can see otherwise.
    #[test]
    fn resolved_params_reports_the_coercion_not_the_input() {
        let get = |name: &str, src: &str, key: &str| -> String {
            let p: Value = toml::from_str(src).unwrap();
            resolved_params(name, &p)
                .unwrap()
                .into_iter()
                .find(|(k, _)| *k == key)
                .unwrap_or_else(|| panic!("{name} reports no {key}"))
                .1
        };
        // `read_rungs` CLAMPS a negative count to zero — a grid that rests nothing.
        assert_eq!(get("grid", "rungs = -5", "rungs"), "0");
        // `PairsZScore` rounds and floors its window at 2.
        assert_eq!(get("pairs_zscore", "period = 1", "period"), "2");
        assert_eq!(get("pairs_zscore", "period = 2.6", "period"), "3");
        // An unrecognised `anchor` silently means `first` — so the echo says `first`.
        assert_eq!(get("grid", "anchor = \"fixd\"\nanchor_price = 42.0", "anchor"), "first");
        // ...as an unrecognised `side` silently means LONG.
        assert_eq!(get("dca_accumulate", "side = \"shrot\"", "side"), "long");
        // A default the profile never mentions is still reported: `venue` is the literal "sim".
        assert_eq!(get("momentum", "qty = 2.0", "venue"), "sim");
        // A `venues` row whose value is not a string is dropped by the reader — the echo shows the
        // map that survived, not the table that was typed.
        assert_eq!(get("momentum", "[venues]\nBTC = 7", "venues"), "(none)");
        assert_eq!(get("momentum", "[venues]\nBTC = \"okx\"", "venues"), "BTC:okx");
        // An un-armed barrier leg says so rather than printing a fake zero.
        assert_eq!(get("momentum", "qty = 1.0", "tp"), "(unarmed)");
        assert_eq!(get("momentum", "tp = 5", "tp"), "5");
        // A REQUIRED symbol left unset is a mount that cannot route — the echo must not render it
        // as an empty string, which reads like a configured value.
        assert!(get("pairs_zscore", "entry_z = 2.0", "symbol_a").contains("unset"));
        assert_eq!(get("pairs_zscore", "symbol_a = \"BTC\"", "symbol_a"), "BTC");
        // An OPTIONAL symbol has THREE states, and the echo must keep them apart. ABSENT is the
        // legal default (take the symbol off the feed); EMPTY is a value the operator STATED that
        // stops the strategy trading outright — proven, not asserted, by
        // `the_empty_symbol_this_echo_reports_really_does_stop_the_strategy` below. Rendering the
        // empty one as `""` (or as the absent one) is the `req_sym` defect wearing its sibling's
        // name, which is exactly how it survived that fix.
        for name in ["buy_hold", "grid", "dca_accumulate", "funding_capture"] {
            assert_eq!(
                get(name, "", "symbol"),
                "(from the feed)",
                "{name}: an ABSENT optional symbol is the working default"
            );
            let empty_sym = get(name, "symbol = \"\"", "symbol");
            assert!(
                empty_sym.contains("cannot trade"),
                "{name}: an EMPTY symbol stops the strategy and the echo must say so, got \
                 {empty_sym:?}"
            );
            assert_ne!(
                empty_sym,
                get(name, "", "symbol"),
                "{name}: empty and absent are different mounts and must not render alike"
            );
            assert_eq!(get(name, "symbol = \"BTCUSDT\"", "symbol"), "BTCUSDT", "{name}");
        }
        // ⚠ The THIRD renderer — `funding_carry`'s inline arm — is deliberately NOT changed, and
        // this row is why: there an empty `symbol` is a REAL, working mode (TWO-LEG delta-neutral;
        // `crates/vike-strategy/src/funding_carry.rs`'s `evaluate` gates on
        // `!self.symbol.is_empty()`), so "cannot trade" would be false about it. Same spelling,
        // opposite meaning — which is the reason the fix is per-renderer rather than a sweep.
        assert_eq!(get("funding_carry", "qty = 1.0", "symbol"), "(both legs)");
    }

    /// The claim `resolved_params`' `opt_sym` now makes about an EMPTY symbol — that the strategy
    /// cannot trade — proven for every strategy it renders.
    ///
    /// ⚠ **This is the half the `req_sym` fix never had, and its absence is why the same defect
    /// survived in the sibling helper.** An echo checked only against its own wording drifts the
    /// moment a reader changes its guard. Delete `symbol.is_empty()` from `BuyHold::buy`, from
    /// either `crates/vike-strategy/src/grid_dca.rs` `drive`, or from
    /// `crates/vike-strategy/src/funding_capture.rs`'s `on_bar`, and this goes red — which is the
    /// only thing keeping the sentence in an operator's mount log true.
    ///
    /// The ABSENT case is the control, and it is load-bearing: without it "empty submits nothing"
    /// would also pass for a bar that trades nothing at all, and the test would prove neither half.
    #[test]
    fn the_empty_symbol_this_echo_reports_really_does_stop_the_strategy() {
        // Carries BOTH a symbol and a funding rate, so one bar drives all four strategies: the grid
        // pair arm their ladders off `close`, and `funding_capture` acts only on a funding bar.
        fn a_bar() -> Bar {
            Bar {
                ts: 1,
                open: 100.0,
                high: 100.0,
                low: 100.0,
                close: 100.0,
                volume: 0.0,
                funding: Some(0.01),
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".to_string()),
            }
        }
        fn submits(name: &str, src: &str) -> usize {
            let params: Value = toml::from_str(src).unwrap();
            let mut s = resolve(name, &params).unwrap_or_else(|e| panic!("{name} resolves: {e}"));
            let mut broker = ProbeBroker::default();
            s.on_bar(&mut broker, &a_bar());
            broker.submits
        }
        for name in ["buy_hold", "grid", "dca_accumulate", "funding_capture"] {
            assert_eq!(
                submits(name, "symbol = \"\""),
                0,
                "{name} routed an order on an EMPTY symbol — the echo's \"cannot trade\" would be \
                 a lie"
            );
            assert!(
                submits(name, "") > 0,
                "{name} routed nothing even with the symbol taken off the bar, so the empty-symbol \
                 assertion above proves nothing about the symbol"
            );
        }
    }

    /// Every declared key must be one the reader ACCEPTS — proven behaviourally for the two arms
    /// whose fields are readable from this crate, so the table is not only text-checked by
    /// `tests/param_keys_gate.rs` but observed to move something.
    #[test]
    fn a_declared_key_actually_moves_the_strategy() {
        let params: Value = toml::from_str("size = 7.5\n").unwrap();
        assert_eq!(BuyHold::from_params(&params).size, 7.5);
        assert!(unknown_params("buy_hold", &params).is_empty());

        let g: Value = toml::from_str("band = 9.0\n").unwrap();
        assert_eq!(Grid::from_params(&g).band, 9.0);
        assert!(unknown_params("grid", &g).is_empty());
    }

    #[test]
    fn buy_hold_from_params_reads_size_and_symbol() {
        let params: Value = toml::from_str("size = 2.5\nsymbol = \"ETHUSDT\"\n").unwrap();
        let strat = BuyHold::from_params(&params);
        assert_eq!(strat.size, 2.5);
        assert_eq!(strat.symbol.as_deref(), Some("ETHUSDT"));
    }

    #[test]
    fn buy_hold_from_params_defaults_size_to_one_and_symbol_to_none() {
        let strat = BuyHold::from_params(&empty());
        assert_eq!(strat.size, 1.0);
        assert_eq!(strat.symbol, None);
    }

    #[test]
    fn buy_hold_from_params_accepts_integer_size() {
        let params: Value = toml::from_str("size = 3\n").unwrap();
        assert_eq!(BuyHold::from_params(&params).size, 3.0);
    }

    #[test]
    fn params_reach_the_resolved_strategy() {
        // The concern the per-arm resolve tests in vike-backtest exist for: a typo'd knob silently
        // falling back to a default. Proven here for the arms whose reader is in THIS crate.
        let g: Value = toml::from_str("step = 0.5\nrungs = 4\nsize = 2.0\nband = 3.0\n").unwrap();
        assert!(resolve("grid", &g).is_ok());
        let grid = Grid::from_params(&g);
        assert_eq!(grid.step, 0.5);
        assert_eq!(grid.rungs, 4);

        let m: Value = toml::from_str("qty = 5.0\ntick_size = 0.01\ngamma = 0.2\n").unwrap();
        assert!(resolve("spread_maker", &m).is_ok());
        assert_eq!(SpreadMaker::from_params(&m).unwrap().params().qty, 5.0);
    }
}
