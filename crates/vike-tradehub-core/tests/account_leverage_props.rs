//! **An ACCOUNT's own leverage reaches its engine's margin rate under random input** (decision 0181):
//! the property twin of `crates/vike-tradehub-core/src/engine/leverage_tests.rs`'s hand-picked rows. The
//! subject is `crates/vike-tradehub-core/src/engine/assemble.rs`'s `assemble_engine` (the stated figure
//! judged by `judge_account_leverage`, then folded into `limits.im_requirement`), driven exactly as the
//! node drives it: `make_engine_for_account` over a scripted venue backend, the engine's own `RiskGate`
//! read back.
//!
//! | law | what would be true without it |
//! |---|---|
//! | [`the_applied_rate_is_never_looser_than_the_stated_figure_a_ceiling_or_the_venues_cap`] | an engine margining below what the venue, `policy.max_leverage` or a profile lets the account use |
//! | [`an_unconfirmed_cap_leaves_exactly_the_1x_default`] | a leverage the venue never confirmed reaching the gate |
//! | [`a_figure_above_the_venues_cap_is_refused_not_clamped`] | the operator asking for X and silently getting the venue's Y |
//! | [`the_rate_is_monotone_in_the_stated_figure_up_to_the_cap`] | a higher stated figure margining an order MORE |
//! | [`a_row_stating_nothing_is_byte_identical_to_the_engine_before_decision_0181`] | an upgrade re-margining every account that never asked |
//! | [`the_gate_demands_exactly_price_times_qty_times_the_rate`] | the buying-power lane drifting from `price x qty x im` |
//! | [`the_live_drill_order_is_denied_at_1x_and_admitted_at_10x`] | the 2026-10-18 hyperliquid drill failing again |
//!
//! ⚠ **FAILURE POLICY: a counterexample here is a REAL margin finding.** Stop and report the minimal
//! case proptest prints, with the seed committed BESIDE this file as `account_leverage_props.proptest-regressions`
//! (the root `CLAUDE.md`'s property-test rule); never bend the assembly to the property or the property
//! to the assembly: this fold sizes real orders.
//!
//! The float bounds carry a relative slack of [`EPS`]: the profile's own figure takes a `1 / (1 / L)`
//! round trip through `im_requirement`, which can move the last bit, and nothing else is inexact. A
//! mutation of a `min`, a `max` or a branch moves the rate by a whole factor, far outside it.
//!
//! Own test binary ON PURPOSE: a proptest regression sidecar disqualifies a file from a grouped binary
//! (`crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation").

use std::collections::{BTreeMap, HashSet};

use proptest::prelude::*;
use vike_config::{ArmingBlock, VenueArming};
use vike_exec::{ExecutionClient, RiskContext, RiskGate};
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_model::{
    AccountTier, ContractValue, OrderRequest, OrderType, ProfileRisk, RiskLimits, TradingState,
};
use vike_tradehub_core::symbol_grid::GridSource;
use vike_tradehub_core::{
    AdapterBind, AdapterEnv, AdapterPolicy, ArmingRow, ArmingTable, EngineError, ScriptedBackend,
    VenueFacts, make_engine_for_account,
};

/// Relative slack on a float bound (see the module doc).
const EPS: f64 = 1e-12;

/// A venue client that does nothing: the laws judge the fold, never an order.
struct NoopClient;

impl ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// The venue whose account leverage the platform can confirm (`LEVERAGE_VENUES`), and one it cannot.
fn confirmable() -> &'static str {
    vike_secrets::LEVERAGE_VENUES[0].venue
}
/// deribit has no leverage number and is never listed (decision 0182); bybit, okx and binance are
/// listed since, and their `Posted` rule has its own scenarios in `src/engine/leverage_tests.rs`.
const UNCONFIRMABLE: &str = "deribit";

/// What the gatewayhub's `AdapterBind::leverage_cap` says.
#[derive(Clone, Debug)]
enum Cap {
    /// No answer (an older gatewayhub, or the venue was not asked).
    Absent,
    /// The venue could not confirm.
    Failed,
    /// The venue confirmed the account may use this much.
    Confirmed(f64),
    /// A confirmation that is not a usable leverage (NaN, infinite, below 1x).
    Unusable(f64),
}

impl Cap {
    fn answer(&self) -> Option<Result<f64, String>> {
        match self {
            Cap::Absent => None,
            Cap::Failed => Some(Err("the venue could not be asked".to_string())),
            Cap::Confirmed(cap) | Cap::Unusable(cap) => Some(Ok(*cap)),
        }
    }
}

/// One engine start: the row, the venue's answer, the two existing leverage ceilings.
#[derive(Clone, Debug)]
struct Scenario {
    venue: &'static str,
    /// The account row's `leverage` (decision 0181), or `None` (nothing stated).
    stated: Option<f64>,
    cap: Cap,
    /// `policy.max_leverage`.
    ceiling: Option<f64>,
    /// A run profile's `[risk] max_leverage`.
    profile: Option<f64>,
}

impl Scenario {
    /// The same start with a different stated figure.
    fn stating(&self, stated: Option<f64>) -> Scenario {
        Scenario { stated, ..self.clone() }
    }
    /// Whether the figure must be APPLIED: the venue is confirmable, it confirmed a usable cap and
    /// the stated figure does not exceed it. Written from the decision, not from the code.
    fn applied(&self) -> Option<f64> {
        let stated = self.stated?;
        match self.cap {
            Cap::Confirmed(cap) if self.venue == confirmable() && stated <= cap => Some(stated),
            _ => None,
        }
    }
    /// The engine's rate BEFORE decision 0181: the profile's `1 / leverage` (narrowed to the box ceiling),
    /// or the 1x the shared tail arms when nothing states one.
    fn rate_without_the_row(&self) -> f64 {
        self.profile.map_or(1.0, |lev| 1.0 / lev.min(self.ceiling.unwrap_or(f64::INFINITY)))
    }
}

fn arming_table(venue: &'static str) -> ArmingTable {
    let facts = BTreeMap::from([(
        venue.to_string(),
        VenueFacts {
            adapter_present: true,
            addresses_accounts: true,
            takes_recon_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_decl_word: "not-wired",
        },
    )]);
    let row = ArmingRow {
        arming: VenueArming {
            venue,
            label: AccountLabel::Default,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block: ArmingBlock::None,
            account_ids: vec![1],
        },
        book: None,
    };
    ArmingTable::new(facts, vec![row])
}

/// An ACTIVE `demo` account row of `venue`, stating `leverage`.
fn account_row(venue: &str, leverage: Option<f64>) -> vike_secrets::Account {
    vike_secrets::Account {
        id: 1,
        venue: venue.to_string(),
        tier: AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: leverage.and_then(vike_secrets::AccountLeverage::new),
    }
}

/// Build the engine `sc` starts, its account row planted in the policy's table. Panics on a refused
/// start: every scenario here is a legal one.
fn engine_of(sc: &Scenario) -> vike_exec::ExecutionEngine<Box<dyn ExecutionClient + Send>> {
    let venue = sc.venue;
    let table = arming_table(venue);
    let bind = AdapterBind {
        client: Box::new(NoopClient),
        recon: None,
        bound_tier: AccountTier::Demo,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: Vec::new(),
        confirmed_book: None,
        leverage_cap: sc.cap.answer(),
        account_ccy: None,
        account_ccy_assumed: false,
        mount_symbol: None,
    };
    let backend = ScriptedBackend::new(table.clone())
        .script_start(&route_key_of(venue, &AccountLabel::Default), Ok(bind));
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let mut policy = AdapterPolicy::default().with_account_row(account_row(venue, sc.stated));
    policy.max_leverage = sc.ceiling;
    let risk = ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        max_leverage: sc.profile,
        ..ProfileRisk::default()
    };
    let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
    env.risk_profile = Some(&risk);
    env.policy = Some(&policy);
    match make_engine_for_account(&mut env, venue, "BTC", &AccountLabel::Default, &[]) {
        Ok((engine, _recon)) => engine,
        Err(EngineError::NotRunning(stopped)) => panic!("the account did not run: {stopped:?}"),
        Err(other) => panic!("the start was refused: {other}"),
    }
}

fn limits_of(sc: &Scenario) -> RiskLimits {
    engine_of(sc).gate.limits.clone()
}

fn rel_close(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPS * b.abs().max(f64::MIN_POSITIVE)
}

// ---------------------------------------------------------------------------------------------
// The generator.
// ---------------------------------------------------------------------------------------------

/// A leverage figure: whole numbers (the ones an operator types) and arbitrary reals, `1..=1000`
/// (`account.leverage`'s CHECK).
fn figure() -> impl Strategy<Value = f64> {
    prop_oneof![(1u32..=100).prop_map(f64::from), 1.0f64..=1000.0]
}

prop_compose! {
    /// A start whose cap is drawn RELATIVE to the stated figure, so that "equal to the cap", "just
    /// above" and "below" are each common rather than a one-in-a-thousand coincidence.
    fn scenarios()(
        base in figure(),
        states_one in prop::bool::weighted(0.85),
        mode in 0u8..7,
        frac in 0.0f64..=1.0,
        unusable in prop::sample::select(vec![f64::NAN, f64::INFINITY, 0.5, 0.0, -3.0]),
        on_a_confirmable_venue in prop::bool::weighted(0.85),
        ceiling in prop::option::of(prop_oneof![figure(), 0.25f64..1.0]),
        profile in prop::option::of(prop_oneof![(1u32..=100).prop_map(f64::from), 1.0f64..=100.0]),
    ) -> Scenario {
        let cap = match mode {
            0 => Cap::Absent,
            1 => Cap::Failed,
            2 => Cap::Confirmed(base),
            3 => Cap::Confirmed(base * (1.0 + frac * 9.0)),
            4 => Cap::Confirmed((base * frac).max(1.0)),
            5 => Cap::Unusable(unusable),
            _ => Cap::Confirmed(1.0 + frac * 99.0),
        };
        // A box ceiling under 1x is nonsense the settings edge refuses; it narrows a profile's own
        // figure under 1x, which the profile loader refuses in turn. The fold only has to survive it
        // beside NO profile figure (and clamp the account's figure at 1x), so that is the only place
        // it is drawn.
        let profile = if ceiling.is_some_and(|c| c < 1.0) { None } else { profile };
        Scenario {
            venue: if on_a_confirmable_venue { confirmable() } else { UNCONFIRMABLE },
            stated: states_one.then_some(base),
            cap,
            ceiling,
            profile,
        }
    }
}

proptest! {
    /// **The applied rate is never looser than the stated figure, a ceiling or the venue's cap.** Whenever
    /// the figure is applied the engine margins at `1 / max(1, min(stated, policy.max_leverage, the
    /// profile's leverage))`: so `im >= 1 / min(stated, ceiling, profile, cap)` (the engine is never
    /// looser than the venue's confirmed cap, because `stated <= cap` is the condition of applying it),
    /// `im <= 1` (an account's figure never margins MORE than the 1x default), and under a profile figure
    /// it never drops below the profile's own rate. Nothing but `im_requirement` moves.
    #[test]
    fn the_applied_rate_is_never_looser_than_the_stated_figure_a_ceiling_or_the_venues_cap(
        sc in scenarios(),
    ) {
        let Some(stated) = sc.applied() else { return Ok(()) };
        let Cap::Confirmed(cap) = sc.cap else { unreachable!("applied() needs a confirmed cap") };
        let limits = limits_of(&sc);
        let im = limits.im_requirement.expect("the buying-power gate is armed");
        let ceiling = sc.ceiling.unwrap_or(f64::INFINITY);
        let profile = sc.profile.unwrap_or(f64::INFINITY);
        let least = stated.min(ceiling).min(profile).min(cap).max(1.0);
        let ideal = 1.0 / stated.min(ceiling).min(profile).max(1.0);
        prop_assert!(rel_close(im, ideal), "applied {im}, expected 1 / min(stated, ceilings) = {ideal}");
        prop_assert!(im >= (1.0 / least) * (1.0 - EPS), "looser than a ceiling or the cap: {im} < 1/{least}");
        prop_assert!(im >= (1.0 / cap) * (1.0 - EPS), "looser than the venue's confirmed {cap}x: {im}");
        prop_assert!(im <= 1.0, "an account's figure margined MORE than 1x: {im}");
        if let Some(lev) = sc.profile {
            let own = 1.0 / lev.min(ceiling);
            prop_assert!(im >= own * (1.0 - EPS), "under the profile's own rate {own}: {im}");
        }
        // Only the margin rate moved: the rest of the limits are the row-less engine's.
        let without = limits_of(&sc.stating(None));
        prop_assert_eq!(limits, RiskLimits { im_requirement: Some(im), ..without });
    }

    /// **An unconfirmed or absent cap gives exactly today's rate**: no answer, a failed one, an unusable
    /// one, or a venue the platform cannot confirm against, whatever the row states. With no profile
    /// figure that is `Some(1.0)` to the bit; with one it is the profile's own `1 / leverage`.
    #[test]
    fn an_unconfirmed_cap_leaves_exactly_the_1x_default(sc in scenarios()) {
        if sc.applied().is_some() {
            return Ok(());
        }
        let limits = limits_of(&sc);
        prop_assert_eq!(
            limits.im_requirement.map(f64::to_bits),
            Some(sc.rate_without_the_row().to_bits()),
            "an unconfirmed figure reached the gate"
        );
        if sc.profile.is_none() {
            prop_assert_eq!(limits.im_requirement, Some(1.0));
        }
        prop_assert_eq!(limits, limits_of(&sc.stating(None)));
    }

    /// **A stated figure above the venue's cap is refused (1x), never clamped to the cap**: the engine is
    /// exactly the row-less one, so the operator who asked for X gets today's rate and the reason, not a
    /// silent Y.
    #[test]
    fn a_figure_above_the_venues_cap_is_refused_not_clamped(
        sc in scenarios(),
        over in 0.001f64..=1.0,
    ) {
        let Cap::Confirmed(cap) = sc.cap else { return Ok(()) };
        // `account.leverage` is CHECKed to 1..=1000 (a larger figure would be no row at all), so the
        // figure is drawn strictly between the cap and 1000: `over` is the fraction of that gap.
        let cap = cap.min(900.0);
        let stated = cap + over * (1000.0 - cap);
        let sc = Scenario { venue: confirmable(), cap: Cap::Confirmed(cap), stated: Some(stated), ..sc };
        let limits = limits_of(&sc);
        prop_assert_eq!(
            limits.im_requirement.map(f64::to_bits),
            Some(sc.rate_without_the_row().to_bits()),
            "a figure above the cap {} was applied or clamped", cap
        );
        prop_assert_eq!(limits, limits_of(&sc.stating(None)));
    }

    /// **The rate is monotone in the stated figure up to the cap**: of two figures the venue confirms, the
    /// higher one never margins an order MORE. (Past the cap the answer jumps back to the row-less rate,
    /// which the law above pins; that is a refusal, not a rate.)
    #[test]
    fn the_rate_is_monotone_in_the_stated_figure_up_to_the_cap(
        sc in scenarios(),
        cap in 1.0f64..=200.0,
        low in 0.0f64..=1.0,
        high in 0.0f64..=1.0,
    ) {
        let (low, high) = (low.min(high), low.max(high));
        let at = |fraction: f64| 1.0 + fraction * (cap - 1.0);
        let sc = Scenario { venue: confirmable(), cap: Cap::Confirmed(cap), ..sc };
        let rate = |fraction: f64| {
            limits_of(&sc.stating(Some(at(fraction)))).im_requirement.expect("the gate is armed")
        };
        let (rate_low, rate_high) = (rate(low), rate(high));
        prop_assert!(
            rate_high <= rate_low * (1.0 + EPS),
            "{}x margins at {rate_high} but {}x at {rate_low}", at(high), at(low)
        );
        prop_assert!(rate_high >= (1.0 / cap) * (1.0 - EPS), "past the confirmed {cap}x");
    }

    /// **A row stating nothing (`none`) is byte-identical to the engine before decision 0181**, whatever
    /// the venue answers and whichever ceilings are armed. (An engine only starts from an account row, so
    /// the pre-0181 engine IS the row that states nothing: the stand-in for it is the same start with no
    /// answer from the venue at all, and the rate is the profile's own `1 / leverage` or the 1x default.)
    /// Compared field for field, through the `Debug` bytes as well as `PartialEq`.
    #[test]
    fn a_row_stating_nothing_is_byte_identical_to_the_engine_before_decision_0181(
        sc in scenarios(),
    ) {
        let nothing = sc.stating(None);
        let asked = limits_of(&nothing);
        let before = limits_of(&Scenario { cap: Cap::Absent, ..nothing.clone() });
        prop_assert_eq!(format!("{asked:?}"), format!("{before:?}"));
        prop_assert_eq!(&asked, &before);
        prop_assert_eq!(
            asked.im_requirement.map(f64::to_bits),
            Some(sc.rate_without_the_row().to_bits())
        );
    }

    /// **The gate's margin demand is `price x qty x im`, to the bit**, for any price, size and rate: an
    /// opening order is admitted when the free buying power covers it and denied `insufficient-margin`
    /// when it falls short, however little. This is the RiskGate half the engine's rate feeds.
    #[test]
    fn the_gate_demands_exactly_price_times_qty_times_the_rate(
        price in prop_oneof![0.01f64..=100_000.0, (1u32..=10_000).prop_map(f64::from)],
        qty in prop_oneof![0.001f64..=1_000.0, (1u32..=100).prop_map(f64::from)],
        leverage in figure(),
        equity_vs_demand in prop::sample::select(vec![0.5, 0.999_999, 1.0, 1.000_001, 2.0]),
    ) {
        let im = 1.0 / leverage;
        let demand = price * qty * im;
        let verdict = RiskGate::new(RiskLimits { im_requirement: Some(im), ..RiskLimits::new() })
            .check(&opening(price, qty), &flat_account(price, demand * equity_vs_demand));
        prop_assert_eq!(
            verdict.ok,
            equity_vs_demand >= 1.0,
            "price {} x qty {} x im {} = {} against {} of equity: {:?}",
            price,
            qty,
            im,
            demand,
            demand * equity_vs_demand,
            verdict
        );
        if !verdict.ok {
            prop_assert_eq!(verdict.reason.as_str(), "insufficient-margin");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The gate, pinned.
// ---------------------------------------------------------------------------------------------

/// A limit BUY of `qty` at `price` on an account with nothing else open.
fn opening(price: f64, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "drill-1".to_string(),
        venue: confirmable().to_string(),
        symbol: "BTC".to_string(),
        order_type: OrderType::Limit,
        side: 1,
        qty,
        price: Some(price),
        ..Default::default()
    }
}

/// The gate's view of a flat account holding `equity`, with the symbol marked at `mark`.
fn flat_account(mark: f64, equity: f64) -> RiskContext {
    RiskContext {
        position_size: 0.0,
        mark_price: mark,
        trading_state: TradingState::Active,
        now_ms: 0,
        equity,
        margin_used: 0.0,
        closing_credit: 0.0,
        contract_value: ContractValue::default(),
        quote_rate: 1.0,
        quote_stale: false,
        account_exposure_excl_order: 0.0,
    }
}

/// The 2026-10-18 live drill, pinned (decision 0181's context): the hyperliquid account (id 11, unified,
/// cross leverage 20x held on the venue) holds 8.8 USDC, and its engine is asked for an order worth
/// 11.57 USD. At 1x the gate needs 11.57 of margin and denies `insufficient-margin`; the account
/// stating 10x (the venue confirming 20x) is admitted, and so are 2x and 20x. The demand is checked to
/// the bit at every figure: admitted at exactly `price x qty x im`, denied one ULP under it.
/// (The coin the drill traded is not recorded; the gate sees only the notional, so it is one unit at
/// 11.57.)
#[test]
fn the_live_drill_order_is_denied_at_1x_and_admitted_at_10x() {
    let (price, qty, equity) = (11.57, 1.0, 8.8);
    let start = |stated: f64| {
        let sc = Scenario {
            venue: confirmable(),
            stated: Some(stated),
            cap: Cap::Confirmed(20.0),
            ceiling: None,
            profile: None,
        };
        engine_of(&sc)
    };
    // (the stated figure, the margin rate the engine must arm, whether 8.8 USDC covers the order)
    for (leverage, rate, covered) in
        [(1.0, 1.0, false), (2.0, 0.5, true), (10.0, 0.1, true), (20.0, 0.05, true)]
    {
        let mut engine = start(leverage);
        assert_eq!(
            engine.gate.limits.im_requirement,
            Some(rate),
            "{leverage}x arms 1 / {leverage}"
        );
        let order = opening(price, qty);
        let verdict = engine.gate.check(&order, &flat_account(price, equity));
        assert_eq!(verdict.ok, covered, "{leverage}x against {equity} USDC: {verdict:?}");
        if !covered {
            assert_eq!(verdict.reason, "insufficient-margin");
        }
        // The exact demand: what the gate asks of the account is price x qty x im.
        let demand = price * qty * rate;
        let mut engine = start(leverage);
        let at_demand = engine.gate.check(&order, &flat_account(price, demand));
        assert!(
            at_demand.ok,
            "{leverage}x must admit an account holding exactly {demand}: {at_demand:?}"
        );
        let mut engine = start(leverage);
        let under = engine.gate.check(&order, &flat_account(price, demand.next_down()));
        assert!(!under.ok, "{leverage}x must deny one ULP under {demand}");
        assert_eq!(under.reason, "insufficient-margin");
    }
}

/// The generator reaches both sides of the venue table, and the unconfirmable venue really is outside it
/// (a venue added to `LEVERAGE_VENUES` that is also `UNCONFIRMABLE` here would make the laws vacuous).
#[test]
fn the_scenarios_draw_a_confirmable_and_an_unconfirmable_venue() {
    assert!(vike_secrets::leverage_venue(confirmable()).is_some());
    assert!(vike_secrets::leverage_venue(UNCONFIRMABLE).is_none());
}
