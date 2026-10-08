//! The PORTABLE strategy registry: a name -> `Box<dyn Strategy<B> + Send>` lookup that is generic
//! over the broker, so the SAME table serves the backtest simulator and the live daemon.
//!
//! ## Why this lives here and not in `vike-backtest`
//! As `vike_backtest::harness::registry` it returned `Box<dyn Strategy<SimBroker>>`, so
//! `vike-tradehub` could not run what it backtested without linking the simulator into the binary
//! that signs real orders. The fix is the BOUND, not a new crate: [`vike_model::HftBroker`] is a
//! sub-trait of [`vike_model::Broker`] that both `vike_sim::SimBroker` and `vike_core::LiveBroker`
//! implement, so one `strategy_by_name<B: HftBroker>` resolves every `impl<B: Broker>` strategy AND
//! the `impl<B: HftBroker>` makers ([`vike_mm::SpreadMaker`]) at either broker.
//!
//! ## What is NOT here, and why that is stated as DATA
//! [`SIMULATOR_ONLY`] names the registry arms that stay in `vike-backtest`, each with its reason:
//! five are `impl Strategy<SimBroker>` BY DESIGN, and two (`CheapNp`, `SportTaker`) are portable
//! tape strategies that submit only from `on_trade_tick`, which the feed of
//! `crates/vike-strategy/tests/param_gates.rs`'s `the_scripted_market_moves_every_strategy` cannot
//! drive. A daemon profile naming one fails with that REASON rather than "unknown strategy";
//! `vike-backtest`'s own registry test pins the table to its retained arms.
//!
//! ## The live-capability table
//! Resolving is not trading. [`LIVE_CAPABLE`] is the per-name live-mount verdict, exhaustive over
//! [`PORTABLE_STRATEGIES`], with a written reason for every row — the per-venue capability-map
//! playbook applied to strategies: declare today's reality, THEN flip rows behind real evidence.
//!
//! ## The params refusals, one child module per table — because a params READER cannot fail
//! Every `from_params` here is a READER, not a schema: an unrecognised key is silently ignored and
//! the knob stays at the strategy's COMPILED DEFAULT — on a mount that signs real orders, a
//! position at a size nobody typed. [`PARAM_KEYS`] (`keys`) declares, per name, the keys its reader
//! reads AND the TOML type each takes, so a consumer can refuse the rest ([`unknown_params`]) and a
//! known key carrying an unusable value ([`mistyped_params`]). ⚠ The two halves are the SAME defect:
//! `and_then(Value::as_…)` yields `None` for a wrong type exactly as it does for an absent key, so
//! `size = "2"` — quoting a number — mounts `size = 1` just as surely as `sizee = 2` does.
//! `crates/vike-strategy/tests/param_keys_gate.rs` scans the readers' source in BOTH directions; a
//! row may decline to enumerate ([`ParamKeys::NotEnumerated`]) only with a written reason.
//!
//! [`PARAM_ROUTES`] (`routes`), because a key can be READ and still configure nothing: a ROUTE key
//! — `symbol`, `venue`, the `venues` table — is read by the strategy and then OVERWRITTEN by the
//! mount. `crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s
//! `resolve_intent_symbol`/`resolve_intent_venue` stamp the MOUNT's own `(venue, symbol)` onto
//! every intent while `any_mount_multi` is false, which is every mount this workspace builds
//! (`vike_mount::MountSpec`'s `legs` is empty on every spec). So a params `symbol` that disagrees
//! with the mount passes both checks above, appears in [`resolved_params`] — and the orders go
//! somewhere else. [`misrouted_params`] is the refusal that makes that state unreachable.
//!
//! [`resolved_params`] (`echo`), because a type check only covers what it rejects: a reader may
//! still clamp, fall back on an unrecognised string, drop a per-ROW value or supply a default the
//! profile never mentions. It re-runs the reader and reports what it LANDED ON — a log that
//! repeats the operator's table is false in exactly the case worth logging.
//!
//! [`PARAM_GATES`] (`gates`) serves the CLASS-CLOSER — a key whose value the strategy HOLDS but
//! never READS because another key sent the reader down a different branch. The closer is a TEST:
//! `crates/vike-strategy/tests/param_gates.rs`'s `an_ungated_key_moves_the_strategy` fails, BY
//! NAME, when a declared key does not change the call trace; the table declares the keys
//! legitimately read only under a stated condition. ⚠ **It is TEST-ONLY DATA and nothing
//! operator-facing reads it** — [`resolved_params`] annotates nothing; [`PARAM_GATES`]' own doc
//! carries why. The same module's `unarmable_params` refuses a table whose ladder rests no order.

use toml::Value;

use vike_model::{Bar, Broker, HftBroker, QuoteTick, SpreadModel, Strategy};

use crate::controller::as_f64;
use crate::{
    Controller, ControllerHarness, DcaAccumulate, FundingCapture, FundingCarryController, Grid,
    MomentumController, PairsZScore, TrailingScalper,
};
use vike_mm::SpreadMaker;

pub(crate) mod echo;
pub(crate) mod gates;
pub(crate) mod keys;
pub(crate) mod routes;

#[cfg(doc)]
use echo::resolved_params;
#[cfg(doc)]
use gates::PARAM_GATES;
#[cfg(doc)]
use keys::{PARAM_KEYS, ParamKeys, mistyped_params, unknown_params};
#[cfg(doc)]
use routes::{PARAM_ROUTES, misrouted_params};

/// Every name [`strategy_by_name`] resolves, kept in sync with its `match` by
/// `registry_lists_every_match_arm`. A SUBSET of `vike_backtest::harness::STRATEGIES` (this
/// roster PLUS [`SIMULATOR_ONLY`]): that roster is the authority for what a BACKTEST profile may
/// name, this one for what a LIVE mount may.
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
/// The first five are `impl Strategy<SimBroker>` by DESIGN (the module doc of
/// `crates/vike-sim/src/ref_strategies.rs` names the simulator machinery each needs), so no
/// registry work makes them live. The last two are portable strategies in this crate that sit here
/// only because no portable row can be declared for a strategy that submits from `on_trade_tick`
/// alone; promoting one is future work, not a law.
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
        "lives in THIS crate now (`crate::strategies::sport_taker`) and is still listed here, for \
         the same reason as `cheap_catch_updown_fair_value`: it submits only from \
         `on_trade_tick` and reads a SIGNAL series the replay harness synthesizes, so no \
         PORTABLE row can be declared for it yet (portable in principle, not yet promoted)",
    ),
];

/// Names that resolve in `vike-backtest`'s registry but are on NEITHER roster — the SCRIPT path.
///
/// `rhai` is DELIBERATELY absent from `vike_backtest::harness::STRATEGIES` (the union of
/// [`PORTABLE_STRATEGIES`] and [`SIMULATOR_ONLY`]): its arm needs a `src` param, so it is not
/// default-resolvable. Before this table a profile naming the script strategy
/// was told it did not EXIST, which is false and sends the operator hunting for a typo. This table
/// is the third row-set [`capability`] consults, so the answer is the true one.
///
/// ⚠ This table classifies the NAME, not the script path. Scripts themselves are LIVE-mountable
/// (`docs/decisions/0024-rhai-strategies-live.md`): `vike-tradehub` mounts a script by PATH —
/// `[strategy] rhai = "<path>"` — never through this registry.
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
     `crates/vike-ops/tests/architecture/named_run_closure_gate.rs`, which walks the whole transitive normal \
     closure of `vike-user-strategies` and forbids `vike-script` anywhere in it; THIS crate is in \
     that closure. The `src` requirement is why it is on no roster. \
     Scripts DO mount live — the daemon takes them by PATH, `[strategy] rhai = \"<path>\"`, \
     never through this registry.",
)];

/// One name's LIVE-MOUNT verdict, with the argument attached to WHICHEVER arm it is.
///
/// ⚠ **The permissive arm carries the argument, and that is the whole point of this type.** A
/// live row means MOUNTABLE BY THE ORDER-SIGNING DAEMON, so it must carry its written `why_safe`
/// exactly as a refusal carries its `blocker` — the idiom
/// `crates/vike-tradehub/src/hot_reload.rs`'s `HotClass::Hot` follows, at 80 characters.
///
/// `crates/vike-cli/src/cmd/init/content.rs` and `crates/vike-user-strategies/src/codegen.rs` call
/// this table a "default-deny posture": for USER strategies `USER_LIVE_CAPABLE` is an opt-in list
/// (absence means sim-only), and this type is what makes the built-ins' posture agree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Mountable on the live core, with the written argument for why that is safe.
    Live {
        /// What makes this strategy's inputs and orders correct on a LIVE mount, specifically.
        /// Length-checked, the same bar `HotClass::Hot` clears.
        why_safe: &'static str,
    },
    /// Mountable, and NOBODY EVER WROTE WHY. A ratchet, not a state to add to: the set is pinned
    /// by `UNARGUED_LIVE` (in this module's `tests`) and may only shrink. It keeps an unreviewed
    /// row VISIBLE.
    LiveUnargued,
    /// It RESOLVES but would not trade, with the missing input named.
    NotLive {
        /// The missing input or unmet precondition — a factual claim somebody can check.
        blocker: &'static str,
    },
}

impl Liveness {
    /// The refusal reason, or `None` for either live arm.
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
/// ⚠ This table exists because the failure mode here is a SILENT NO-OP: a strategy whose input
/// never arrives mounts cleanly, logs nothing unusual, and simply never submits — discovered days
/// later. Every "no" below names the missing input, so flipping a row is a factual claim somebody
/// can check rather than an opinion.
///
/// Exhaustive over [`PORTABLE_STRATEGIES`] by `live_capable_table_is_exhaustive`.
pub const LIVE_CAPABLE: &[(&str, Liveness)] = &[
    // The three symbol-inferring strategies: safe because bars reaching a live mount ARE
    // symbol-stamped — argued PER ROW, so an unargued row cannot hide under a shared comment.
    (
        "buy_hold",
        Liveness::Live {
            why_safe: "bars reaching a live mount are symbol-stamped — \
                       `crates/vike-core/src/runtime/dispatch.rs`'s `BarClose` arm sets \
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
    /// Not in this registry: it belongs to `vike-backtest`'s registry — the reason is from
    /// [`SIMULATOR_ONLY`] or [`SCRIPT_ONLY`], one variant because both mean "it backtests; this
    /// binary does not link what runs it" to a consumer about to mount.
    SimulatorOnly(&'static str),
    /// No such strategy anywhere.
    Unknown,
}

/// Classify `name` for a caller about to MOUNT it — the daemon's profile validation: "typo",
/// "simulator-only" or "resolves but cannot trade live", at profile-LOAD time rather than from a
/// mount that quietly never trades.
pub fn capability(name: &str) -> Capability {
    if let Some((_, why)) = SIMULATOR_ONLY.iter().chain(SCRIPT_ONLY).find(|(n, _)| *n == name) {
        return Capability::SimulatorOnly(why);
    }
    match LIVE_CAPABLE.iter().find(|(n, _)| *n == name) {
        // ⚠ `LiveUnargued` answers `Live` here, deliberately: it describes what the daemon WILL do
        // today, not what anybody reviewed. Answering `NotLive` would be a behaviour change
        // smuggled in behind a type; `UNARGUED_LIVE` (this module's `tests`) keeps the gap visible.
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
    /// The name resolved but its params are unusable — the `spread_maker`/`gueant_maker` arms when
    /// a `[strategy.params]` enum key is PRESENT and names no variant (`vike_mm::ParamError`), so a
    /// typo'd `spread_model` fails at load. Carries that reader's own message.
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
/// `Box<dyn Strategy<LiveBroker> + Send>` — the live core moves the strategy onto its own thread;
/// it unsize-coerces to the simulator's `Box<dyn Strategy<SimBroker>>`. Unknown names are a
/// [`RegistryError::Unknown`], so a typo'd `strategy.name` fails at load.
pub fn strategy_by_name<B: HftBroker + 'static>(
    name: &str,
    params: &Value,
) -> Result<Box<dyn Strategy<B> + Send>, RegistryError> {
    match name {
        "buy_hold" => Ok(Box::new(BuyHold::from_params(params))),
        // The grid family (`crate::strategies::grid_dca`): portable and param-driven — a sweep
        // grids over `step`/`band`/`size`/`rungs` — so they follow the `BuyHold` reader convention.
        "grid" => Ok(Box::new(Grid::from_params(params))),
        "dca_accumulate" => Ok(Box::new(DcaAccumulate::from_params(params))),
        // The vike-mm Avellaneda–Stoikov maker, at ANY `B: HftBroker` (the bound this registry
        // exists for). UNLIKE every other arm it trades ONLY on a TICK slice: it overrides
        // `on_quote_tick`/`on_order_book`, NOT `on_bar`, so a bar-only mount never quotes.
        // ⚠ `SpreadMaker::from_params` FAILS on a present-but-unrecognized enum value
        // (`spread_model`/`kappa_mode`/`style`) — hence `BadParams`, not a swallowed fallback. An
        // ABSENT key still keeps its default.
        "spread_maker" => Ok(Box::new(spread_maker(params)?)),
        // The GLFT maker: `spread_maker` forced to `SpreadModel::Gueant`, reading the SAME knobs —
        // a roster alias for `spread_model = "gueant"`.
        "gueant_maker" => {
            Ok(Box::new(spread_maker(params)?.with_spread_model(SpreadModel::Gueant)))
        }
        // The Polymarket two-buy / delayed-flatten scalper: rests buy-Up + buy-Down, on a fill
        // cancels the opposite and flattens the held side after `exit_delay_ms`. TICK lane; keep
        // the engine order-latency OFF (the delay is in the strategy).
        "trailing_scalper" => Ok(Box::new(TrailingScalper::from_params(params))),
        // The two CONTROLLERS, each wrapped in the portable `ControllerHarness`:
        // `controller_harness` reads the harness-level knobs, each `from_params` the controller's.
        "momentum" => {
            Ok(Box::new(controller_harness(MomentumController::from_params(params), params)))
        }
        // Funding-rate CARRY, MULTI-VENUE: `[strategy.params.venues]` routes each series to its OWN
        // venue so ONE mount fills the cross-venue funding book; an empty `symbol` is the TWO-LEG
        // pair. ⚠ A LIVE mount declares no legs and no live lane carries a funding rate — see
        // [`LIVE_CAPABLE`].
        "funding_carry" => {
            Ok(Box::new(controller_harness(FundingCarryController::from_params(params), params)))
        }
        // The DIRECTIONAL funding-HARVEST strategy: reads `Bar::funding` (fed by the backtest's
        // `engine.attach_funding`), which is also why it is a `no` row in [`LIVE_CAPABLE`].
        "funding_capture" => Ok(Box::new(FundingCapture::from_params(params))),
        // The COST-GATED pairs instrument. TWO-LEG (`symbol_a` + `symbol_b`, no single-symbol
        // fallback); its entry must EXCEED a full round trip INCLUDING CARRY, so
        // `half_spread_bps`/`taker_fee`/`hold_intervals` are load-bearing knobs.
        "pairs_zscore" => Ok(Box::new(PairsZScore::from_params(params))),
        other => Err(RegistryError::Unknown(other.to_string())),
    }
}

/// The shared body of the `spread_maker` and `gueant_maker` arms: `vike_mm`'s param failure becomes
/// `RegistryError::BadParams`, its message (key, value, accepted spellings) unchanged.
fn spread_maker(params: &Value) -> Result<SpreadMaker, RegistryError> {
    SpreadMaker::from_params(params).map_err(|e| RegistryError::BadParams(e.to_string()))
}

/// Wrap a [`Controller`] in its portable [`ControllerHarness`], reading the harness-level knobs
/// (NOT controller knobs) from the same params table: `venue` (default `"sim"`; the key a
/// `funding_carry` controller reads to pick its leg), `cooldown_ms` (post-exit re-open spacing,
/// default `0`) and the `venues` map. The controller's OWN knobs were read by its `from_params`.
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

/// Optional per-symbol venue routing: a `[strategy.params.venues]` table (`SYMBOL = "venue"`) lets
/// a CROSS-VENUE controller (`funding_carry`) observe each series under its OWN venue from ONE
/// mount. Absent ⇒ every symbol routes to the default `venue`.
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
/// or the first quote tick) and hold — the registry's end-to-end smoke fixture. PORTABLE
/// (`impl<B: Broker> Strategy<B>`), so it runs unchanged on the live stack; this is its ONE name
/// (docs/decisions/0087).
pub struct BuyHold {
    /// Units to buy (raw, not notional — mirrors [`Broker::submit_market`]'s `qty`).
    pub size: f64,
    /// Explicit symbol override. `None` uses whatever symbol the first bar/tick carries (the
    /// harness's single-symbol path).
    pub symbol: Option<String>,
    bought: bool,
}

impl BuyHold {
    /// `size` defaults to `1.0`; `symbol` is optional. Unrecognized/missing keys are ignored — a
    /// params READER, not a strict schema.
    pub fn from_params(params: &Value) -> Self {
        let size = params.get("size").and_then(as_f64).unwrap_or(1.0);
        let symbol = params.get("symbol").and_then(Value::as_str).map(str::to_string);
        BuyHold { size, symbol, bought: false }
    }

    /// Construct directly (the test/`Default`-ish path the params reader lowers into).
    pub fn new(size: f64, symbol: Option<String>) -> Self {
        BuyHold { size, symbol, bought: false }
    }

    /// The one market buy. ⚠ An EMPTY symbol submits nothing — the claim the resolved echo's
    /// "cannot trade" rendering makes, pinned by its test.
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
mod tests;
