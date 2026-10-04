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
//! [`vike_model::Broker`], and BOTH `vike_sim::SimBroker` (`engine::sim_broker`'s
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
//! (`vike_mount::MountSpec`'s `legs` is empty on every spec). So a params `symbol` that disagrees with
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
/// by DESIGN (their own module doc in `crates/vike-sim/src/ref_strategies.rs` names the
/// simulator machinery each needs — the `SimBroker::symbols`/`schedule` FIELDS, the
/// `crate::sizing::PositionSizer` sentinel path, the cash-gate `weight` argument), so no registry
/// work makes them live. The last two are genuinely portable `impl<B: Broker>` strategies that
/// merely live in the simulator crate because they reach its crate-internal modules
/// (`crate::strategies::cheap_np_ask` / `crate::strategies::fair_value`); moving them is future work, not a law.
///
/// `crates/vike-backtest/src/harness/registry_tests.rs`'s `simulator_only_table_names_the_retained_arms`
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
        "lives in THIS crate now (`crate::strategies::cheap_np`) and is still listed here, which is not a \
         contradiction: this table's job is to say why a name the backtest registry owns has no \
         PORTABLE row, and promoting this one needs a `PARAM_KEYS` entry that fails a gate in \
         BOTH spellings — `Declared` requires it to submit under `param_gates`' bar/quote feed \
         and it submits only from `on_trade_tick`, while `NotEnumerated` requires it in \
         `vike_tradehub`'s `AS_MAKER_NAMES`. Its previous reason — reaching crate-internal \
         modules — is what the move removed",
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
/// `crates/vike-backtest/src/harness/registry_tests.rs`'s
/// `script_only_names_resolve_there_and_not_here` is the two-direction gate.
pub const SCRIPT_ONLY: &[(&str, &str)] = &[(
    "rhai",
    "the SCRIPT path, by NAME: this arm compiles a `vike_script::RhaiStrategy` from an inline \
     `src` param and lives in `vike-backtest`. ⚠ The reason it can live there and NOT here changed \
     on 2026-09-23: `vike-script` used to sit at the SAME layer rank as this crate, so the layer \
     gate refused the edge on every PR. It moved to `domain` (20), so by rank that edge is now \
     legal — and what refuses it instead is \
     `crates/vike-ops/tests/named_run_closure_gate.rs`, which walks the whole transitive normal \
     closure of `vike-user-strategies` and forbids `vike-script` anywhere in it; THIS crate is in \
     that closure. The `src` requirement is why it is on no roster. \
     Scripts DO mount live — the daemon takes them by PATH, `[strategy] rhai = \"<path>\"`, \
     never through this registry.",
)];

/// One name's LIVE-MOUNT verdict, with the argument attached to WHICHEVER arm it is.
///
/// ⚠ **The permissive arm carries the argument, and that is the whole point of this type.** Until
/// 2026-09-20 this was `Option<&str>`: a `Some(reason)` had to explain the refusal and had a
/// length check behind it, while `None` — the shortest thing anybody can type — meant MOUNTABLE BY
/// THE ORDER-SIGNING DAEMON and required nothing at all. Five of the six permissive rows did carry
/// a written argument, in prose above them; one did not, and nothing could tell the two apart.
///
/// The idiom is not invented here. `crates/vike-tradehub/src/hot_reload.rs`'s `HotClass::Hot`
/// already does exactly this — "`reason` is the WRITTEN argument for why that is safe — required
/// at the row, the `LIVE_CAPABLE` idiom" — and enforces 80 characters. It named this table as its
/// model while being stricter than it. This type makes the model match the copy.
///
/// ⚠ Three places call this table a "default-deny posture"
/// (`crates/vike-cli/src/cmd/init/content.rs`, `crates/vike-user-strategies/src/codegen.rs` and
/// that crate's `lib.rs`). For USER strategies it is one — `USER_LIVE_CAPABLE` is an opt-in list
/// and absence means sim-only. For the built-ins it was the opposite, and this type is what makes
/// the two postures agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Mountable on the live core, with the written argument for why that is safe.
    Live {
        /// What makes this strategy's inputs and orders correct on a LIVE mount, specifically.
        /// Length-checked, the same bar `HotClass::Hot` clears.
        why_safe: &'static str,
    },
    /// Mountable, and NOBODY EVER WROTE WHY. A ratchet, not a state to add to: the set is pinned
    /// by `UNARGUED_LIVE` (in this module's `tests`) and may only shrink. It exists so an
    /// unreviewed row is VISIBLE rather than indistinguishable from a reviewed one, which is
    /// what `None` used to make it.
    LiveUnargued,
    /// It RESOLVES but would not trade, with the missing input named.
    NotLive {
        /// The missing input or unmet precondition — a factual claim somebody can check.
        blocker: &'static str,
    },
}

impl Liveness {
    /// The refusal reason, or `None` for either live arm — the shape the call sites had when this
    /// was an `Option<&str>`, so they read the same.
    pub fn blocker(&self) -> Option<&'static str> {
        match self {
            Liveness::NotLive { blocker } => Some(blocker),
            _ => None,
        }
    }

    /// Is this name mountable on the live core? True for BOTH live arms — an unargued row is still
    /// live today, which is exactly why it is worth seeing.
    pub fn is_live(&self) -> bool {
        self.blocker().is_none()
    }
}

/// Per-name LIVE-MOUNT verdict. See [`Liveness`] for why the permissive arm carries an argument.
///
/// ⚠ This table exists because the failure mode here is a SILENT NO-OP, not a compile error or a
/// panic: a strategy whose input never arrives mounts cleanly, logs nothing unusual, and simply
/// never submits. That is the live-vs-backtest divergence class this repo keeps a ledger of, and it
/// is precisely the class an operator discovers days later. Every "no" below names the missing
/// input, so flipping a row is a factual claim somebody can check rather than an opinion.
///
/// Exhaustive over [`PORTABLE_STRATEGIES`] by `live_capable_table_is_exhaustive`.
pub const LIVE_CAPABLE: &[(&str, Liveness)] = &[
    // The three symbol-inferring strategies. Bars reaching a live mount ARE symbol-stamped, which
    // is what makes them safe there — spelled PER ROW now, because a shared comment is exactly what
    // let the one unargued row below hide among them.
    (
        "buy_hold",
        Liveness::Live {
            why_safe: "bars reaching a live mount are symbol-stamped — \
                       `crates/vike-core/src/runtime/mod.rs`'s `BarClose` arm sets \
                       `bar.symbol = Some(key.1)` before `drive_strategy`, so this strategy routes \
                       to the mounted instrument rather than through the empty symbol",
        },
    ),
    (
        "grid",
        Liveness::Live {
            why_safe: "same symbol-stamping guarantee as `buy_hold`: `vike_core::runtime`'s \
                       `BarClose` arm stamps `bar.symbol` before `drive_strategy`, and this \
                       strategy's resting legs are single-venue single-symbol, so nothing it \
                       submits needs a leg the mount does not declare",
        },
    ),
    (
        "dca_accumulate",
        Liveness::Live {
            why_safe: "same symbol-stamping guarantee as `buy_hold`, and strictly one-sided: it \
                       only ever BUYS, so it needs no short inventory on the venue and cannot hit \
                       the naked-short problem that keeps `trailing_scalper` off the live core",
        },
    ),
    (
        "spread_maker",
        Liveness::Live {
            why_safe: "mountable, but the daemon does NOT resolve it through this arm and that is \
                       what makes it safe. `SpreadMaker::from_params` reads `[strategy.params]` \
                       and NOTHING else, so on a profile carrying its own maker fields it would \
                       mount `qty = 1`, `tick_size = 0` and the Bernoulli [0,1] wall clamp — the \
                       exact configuration that posted ZERO orders on a $64k hyperliquid asset \
                       before `vike_mount::MakerMountConfig::crypto` existed. \
                       `vike_tradehub::config::DaemonProfile::resolve_strategy` builds it from the \
                       profile's own maker fields through `vike_mount::build_maker` — the SAME call \
                       the absent-`[strategy]` default path makes — and REFUSES a \
                       `[strategy.params]` table outright, so the named and default spellings are \
                       ONE construction that cannot differ. This arm is the BACKTEST's, where \
                       `[strategy.params]` is the only configuration that exists",
        },
    ),
    (
        "gueant_maker",
        Liveness::Live {
            why_safe: "the same construction and the same refusal as `spread_maker`: the daemon \
                       reaches it through `vike_mount::build_maker` from the profile's own maker \
                       fields and refuses a `[strategy.params]` table, so this arm's \
                       `from_params` reading is not what a live mount gets",
        },
    ),
    (
        "trailing_scalper",
        Liveness::NotLive {
            blocker: "NAKED SHORT on its own venue: from flat it rests a SELL leg documented as \
                      the single-token synthetic of `buy Down` (see its module doc), an \
                      equivalence that holds in the simulator — where a signed position may go \
                      negative for free — and NOT on the Polymarket CLOB it was written for, \
                      which requires the ERC-1155 outcome balance to sell. Live from flat that \
                      leg cannot rest, so the strategy degrades to a one-sided buyer while the \
                      backtest fills both sides; nothing in the mount path converts a sell into \
                      the Down-buy it is priced as",
        },
    ),
    // ⚠ The one row nobody argued. See `UNARGUED_LIVE` in this module's `tests` — a named
    // debt, not a verdict.
    ("momentum", Liveness::LiveUnargued),
    (
        "funding_carry",
        Liveness::NotLive {
            blocker: "TWO-LEG and cross-venue: needs one declared mount leg per carry venue, and \
                      no mount surface declares legs today (`vike_mount::MountSpec`'s `legs` is \
                      empty on every spec, and the paper builder asserts it) — plus the same \
                      missing live funding series as `funding_capture`",
        },
    ),
    (
        "funding_capture",
        Liveness::NotLive {
            blocker: "reads `Bar::funding`, which every live kline lane leaves `None` \
                      (`vike_bridge_core::klines::kline_to_bar`) — it would mount and never see a \
                      funding rate",
        },
    ),
    (
        "pairs_zscore",
        Liveness::NotLive {
            blocker: "TWO-LEG: needs `symbol_a` + `symbol_b` as declared mount legs admitted into \
                      the venue engine's `extra_symbols`; a mount declares no legs today, so the \
                      second leg's bars never arrive and its orders would be dropped at \
                      `ExecutionEngine::accepts_symbol`",
        },
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
/// false for every mount that exists, because `vike_mount::MountSpec`'s `legs` is `Vec::new()` on
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
///     state (`crates/vike-strategy/src/strategies/trailing_scalper.rs`'s batch tool ships half of an inert
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
    /// `crates/vike-strategy/src/strategies/funding_capture.rs`'s `on_bar` returns with the comment "SimBroker
    /// panics on an empty symbol — never route through", and BOTH of
    /// `crates/vike-strategy/src/strategies/grid_dca.rs`'s `drive` methods open with the same guard. The test
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
        // ⚠ `LiveUnargued` answers `Live` here, deliberately: it describes what the daemon WILL do
        // today, not what anybody reviewed. Making it answer `NotLive` would be a behaviour change
        // smuggled in behind a type — see `UNARGUED_LIVE` in this module's `tests`, which
        // exists to make the gap visible rather than to close it silently.
        Some((_, verdict)) => match verdict.blocker() {
            None => Capability::Live,
            Some(why) => Capability::NotLive(why),
        },
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
/// implementing [`HftBroker`] — `vike_sim::SimBroker` for a backtest,
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
        // The two grid-family reference strategies (`crate::strategies::grid_dca`). Both are portable
        // `impl<B: Broker> Strategy<B>` (the `buy_hold` shape) and genuinely param-driven — a sweep
        // grids over `step`/`band`/`size`/`rungs` via `strategy.params` — so they follow the
        // `BuyHold` reader convention rather than `Default::default()`.
        "grid" => Ok(Box::new(Grid::from_params(params))),
        "dca_accumulate" => Ok(Box::new(DcaAccumulate::from_params(params))),
        // The vike-mm Avellaneda–Stoikov market maker. Resolvable at ANY `B: HftBroker`, which is
        // what this whole registry's bound exists for: the same box mounts on `SimBroker`
        // (proven by vike-backtest's `tests/maker_backtest.rs`) and on `LiveBroker` (the mount
        // `vike_mount::build_live_maker_core` has always built). UNLIKE every other arm it trades
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
        // The Polymarket-native two-buy / delayed-flatten scalper (`crate::strategies::trailing_scalper`):
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
        // The DIRECTIONAL funding-HARVEST reference strategy (`crate::strategies::funding_capture`): a
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
/// for, and this is its ONE name: `vike_backtest::harness` re-exported it (twice) so the old path
/// "still resolves" until 2026-09-27, when that alias was retired under the root `CLAUDE.md`'s
/// one-name rule (docs/decisions/0087).
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

#[path = "registry_tests.rs"]
#[cfg(test)]
mod registry_tests;
