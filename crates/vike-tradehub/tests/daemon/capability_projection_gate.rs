//! **Every core order capability either reaches the wire or says why it does not.**
//!
//! The defect this closes was measured rather than feared: `vike_execution` carried `Bracket`, `Combo`,
//! `ArmConditional`, `SubmitBatch` and `CancelBatch`, and `vike_tradehub_client::wire::CommandWire`
//! carried none of them — so a capability a strategy can use was one no remote operator could
//! reach, and nothing was obliged to notice. `Bracket` has since reached the wire (`ON_THE_WIRE`,
//! the default account only); the rest are rows below. This is the venue-roster idiom
//! (`vike_model::VENUES` completeness) applied to CAPABILITY instead of to venue.
//!
//! STEP 1 of the two-step playbook: it merges declaring today's reality, with every gap as a row
//! carrying its own reason. STEP 2 flips rows to mappings one at a time.

use vike_execution::recon::{ReconcileBalanceTolerance, ReconcilePolicy};
use vike_execution::{
    Command, ConditionalIntent, EmulatedEnd, EmulatedStep, EmulatedVerb, MarginUpdate, MountSpec,
    OrderIntent, ParamsUpdate, ReconcileReports, RoutedOrder, SymbolAdmission,
};
use vike_model::TradingState;
use vike_model::{
    ExerciseSpec, OrderBracket, OrderCombo, OrderComboLeg, OrderList, QuoteStyle,
    SpreadMakerParams, StrategyParams, SymbolGrid,
};
// The `CommandWire` twin of `order_intent_name`/`command_name`, one layer down: its exhaustive match is
// what a NEW wire variant breaks, and it is compiled once in vike-tradehub-client for this gate and
// vike-cli's (`crates/vike-tradehub-client/src/test_support/wire_command_names.rs`).
use vike_tradehub_client::test_support::wire_command_names::every_wire_command_variant_name;

/// A core capability the wire deliberately does not carry YET, and why.
///
/// ⚠ A row here is a written admission, never a silencer. Deleting a row is what STEP 2 looks like;
/// adding one needs the reason to argue for itself.
const NOT_ON_THE_WIRE: &[(&str, &str)] = &[
    (
        "OrderIntent::SubmitBatch",
        "no wire variant. `trade order batch --file` sends N sequential Submits behind one confirm \
         and says so; atomicity is what the wire variant would buy. \
         docs/superpowers/specs/2026-09-21-trade-cli-surface-design.md §15.",
    ),
    (
        "OrderIntent::CancelBatch",
        "no wire variant. `mass-cancel` covers the venue/symbol-scoped case, which is the one an \
         operator reaches for; an order client id list has no spelling yet.",
    ),
    (
        "OrderIntent::Confirm",
        "the confirm-grace watchdog's own prod, raised by the engine rather than by an operator. \
         There is no operator question it answers.",
    ),
    (
        "OrderIntent::Exercise",
        "no wire variant. Every venue refuses an option exercise (decision 0234 keeps IBKR's option \
         design closed; deribit's options are European and exercise themselves at expiry; alpaca is \
         deferred), so a remote verb could only ever be refused; the backtest's is the simulator's own.",
    ),
    (
        "OrderIntent::ArmConditional",
        "no wire variant. Conditional arming is a strategy capability today.",
    ),
    (
        "OrderIntent::DisarmConditional",
        "no wire variant, for the same reason as ArmConditional - the pair moves together.",
    ),
    (
        "Command::SetMargin",
        "an initial-margin-requirement knob of the local margin model, not a venue leverage call. \
         An operator verb for it would imply the venue call, which does not exist as a command.",
    ),
    (
        "Command::AdmitSymbol",
        "raised by the node when an account first trades a symbol, after it has resolved that \
         symbol's grid from the venue (decision 0173). Not an operator act: the operator sends the \
         ORDER, and a symbol the engine does not trade is admitted for it or refused. Its one \
         operator-facing form is the RELEASE statement (`SymbolAdmission::release`), the lowering of \
         `CommandWire::UnadmitSymbol` (decision 0205): the same record with one more key, so no \
         `Command` variant of its own exists.",
    ),
    (
        "Command::HoldAccount",
        "raised by the node's account watcher to hold a lane twin it keeps up only to be closed \
         closing-only (a floor a node-wide state cannot loosen), and released when the twin is flat or \
         its row returns. Not an operator act: the operator's per-account word is \
         `Command::SetAccountTradingState` (`trade account`).",
    ),
    (
        "Command::OrderOn",
        "the boot restore's REPLAY shape: it lowers a re-applied strategy submit, conditional fire or \
         margin-call release onto the engine its journal record names (decision 0206). Nothing live \
         sends it and no operator can mean it: the operator names an account on the order itself.",
    ),
    (
        "Command::Emulated",
        "one step of an emulated order (decision 0275), raised only by the live core's own drain-loop \
         boundary and journaled so a replay re-applies it. Not an operator act: the operator sends the \
         pegged or visible-size ORDER (`CommandWire::Submit`) and cancels it by its id.",
    ),
    ("Command::ReconcileReports", "reconcile plumbing, raised by the ReconcileDriver."),
    ("Command::Shutdown", "the process lifecycle, not the order plane."),
];

/// The variant name every arm of the exhaustive matches below reports itself as.
fn order_intent_name(intent: &OrderIntent) -> &'static str {
    match intent {
        OrderIntent::Submit(_) => "OrderIntent::Submit",
        OrderIntent::SubmitBatch(_) => "OrderIntent::SubmitBatch",
        OrderIntent::Cancel(_) => "OrderIntent::Cancel",
        OrderIntent::CancelBatch(_) => "OrderIntent::CancelBatch",
        OrderIntent::Modify { .. } => "OrderIntent::Modify",
        OrderIntent::Confirm(_) => "OrderIntent::Confirm",
        OrderIntent::MassCancel { .. } => "OrderIntent::MassCancel",
        OrderIntent::Flatten { .. } => "OrderIntent::Flatten",
        OrderIntent::MarketExit { .. } => "OrderIntent::MarketExit",
        OrderIntent::Bracket(_) => "OrderIntent::Bracket",
        OrderIntent::ArmConditional(_) => "OrderIntent::ArmConditional",
        OrderIntent::DisarmConditional { .. } => "OrderIntent::DisarmConditional",
        OrderIntent::Combo(_) => "OrderIntent::Combo",
        OrderIntent::OrderList(_) => "OrderIntent::OrderList",
        OrderIntent::Exercise(_) => "OrderIntent::Exercise",
    }
}

/// The [`Command`] twin of [`order_intent_name`]. `Command::Order` DELEGATES to
/// [`order_intent_name`] rather than reporting itself under its own name — the order-scoped verbs
/// it wraps are already the rows [`NOT_ON_THE_WIRE`]/`ON_THE_WIRE` classify; `Command::Order` is
/// not a second capability alongside them.
fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Order(intent) => order_intent_name(intent),
        Command::OrderOn(_) => "Command::OrderOn",
        Command::Emulated(_) => "Command::Emulated",
        Command::UpdateParams(_) => "Command::UpdateParams",
        Command::SetTradingState(_) => "Command::SetTradingState",
        Command::ResumeVenue { .. } => "Command::ResumeVenue",
        Command::SetAccountTradingState { .. } => "Command::SetAccountTradingState",
        Command::HoldAccount { .. } => "Command::HoldAccount",
        Command::SetMargin(_) => "Command::SetMargin",
        Command::AdmitSymbol(_) => "Command::AdmitSymbol",
        Command::ReconcileReports(_) => "Command::ReconcileReports",
        Command::ConfirmReconcile(_) => "Command::ConfirmReconcile",
        Command::CancelReconcileOrder(_) => "Command::CancelReconcileOrder",
        Command::MountStrategy(_) => "Command::MountStrategy",
        Command::UnmountStrategy { .. } => "Command::UnmountStrategy",
        Command::UnlatchMount { .. } => "Command::UnlatchMount",
        Command::Shutdown => "Command::Shutdown",
    }
}

/// The variant names that DO reach the wire, one row per mapping: the core name, and the
/// [`CommandWire`] variant name it maps to.
///
/// ⚠ The second element is a CLAIM, not decoration — `every_on_the_wire_row_names_a_real_wire_variant`
/// checks it against [`every_wire_command_variant_name`], so a row naming a `CommandWire` variant
/// that gets renamed or deleted goes red here instead of silently rotting into a lie.
const ON_THE_WIRE: &[(&str, &str)] = &[
    ("OrderIntent::Submit", "CommandWire::Submit"),
    ("OrderIntent::Cancel", "CommandWire::Cancel"),
    ("OrderIntent::Modify", "CommandWire::Modify"),
    ("OrderIntent::MassCancel", "CommandWire::MassCancel"),
    ("OrderIntent::Flatten", "CommandWire::Flatten"),
    ("OrderIntent::MarketExit", "CommandWire::MarketExit"),
    ("OrderIntent::Bracket", "CommandWire::Bracket"),
    ("OrderIntent::OrderList", "CommandWire::OrderList"),
    ("OrderIntent::Combo", "CommandWire::Combo"),
    ("Command::UpdateParams", "CommandWire::UpdateParams"),
    ("Command::SetTradingState", "CommandWire::SetTradingState"),
    ("Command::MountStrategy", "CommandWire::MountStrategy"),
    ("Command::UnmountStrategy", "CommandWire::UnmountStrategy"),
    ("Command::ConfirmReconcile", "CommandWire::ReconcileConfirm"),
    ("Command::CancelReconcileOrder", "CommandWire::ReconcileCancel"),
    ("Command::ResumeVenue", "CommandWire::ResumeVenue"),
    ("Command::SetAccountTradingState", "CommandWire::SetAccountTradingState"),
    ("Command::UnlatchMount", "CommandWire::UnlatchMount"),
];

#[test]
fn every_order_intent_is_mapped_or_excused() {
    // The exhaustive match is the gate: a NEW variant fails to compile here until its author
    // classifies it. The assertion below is the second half - a variant that compiles but is in
    // neither table is a hole.
    let excused: std::collections::BTreeSet<&str> =
        NOT_ON_THE_WIRE.iter().map(|(name, _)| *name).collect();
    let mapped: std::collections::BTreeSet<&str> =
        ON_THE_WIRE.iter().map(|(core_name, _)| *core_name).collect();

    for name in every_order_intent_variant_name().into_iter().chain(every_command_variant_name()) {
        assert!(
            excused.contains(name) || mapped.contains(name),
            "{name} reaches no CommandWire and carries no NOT_ON_THE_WIRE reason. Either map it \
             (and add it to ON_THE_WIRE) or add a row saying why an operator cannot reach it."
        );
    }
}

/// **`ON_THE_WIRE`'s wire half is a CLAIM, and this is what checks it.** Without this test the
/// file imported nothing from `vike_tradehub_client::wire` at all: every row's second element was
/// an unverified string, so deleting `CommandWire::MountStrategy` tomorrow would leave that row's
/// claim silently false while `every_order_intent_is_mapped_or_excused` stayed green — the gate
/// would defend against a NEW unclassified capability but not against a MAPPED claim rotting.
#[test]
fn every_on_the_wire_row_names_a_real_wire_variant() {
    let real_wire_variants: std::collections::BTreeSet<&str> =
        every_wire_command_variant_name().into_iter().collect();

    for (core_name, wire_name) in ON_THE_WIRE {
        assert!(
            real_wire_variants.contains(wire_name),
            "{core_name} claims to map to {wire_name}, but CommandWire carries no such variant \
             (any more, or ever) - the mapping has rotted into a lie. Fix the row or the wire."
        );
    }
}

/// Every [`OrderIntent`] variant name, produced by constructing one of each and asking
/// [`order_intent_name`].
///
/// ⚠ **This Vec IS hand-maintained, and an earlier version of this doc overstated that away.** The
/// exhaustive match in `order_intent_name` is what a NEW variant breaks — adding one forces a new
/// match arm, or the crate does not compile. This function only makes the ALREADY-LISTED arms
/// actually RUN: nothing forces a new arm's construction into this Vec, so an author can satisfy
/// the compiler up there and still forget to add the variant down here, leaving it permanently
/// unexercised with no failure anywhere. `every_constructed_variant_is_counted_exactly_once` is the
/// backstop — it compares this Vec's length (plus [`every_command_variant_name`]'s) against the
/// total row count across [`ON_THE_WIRE`]/[`NOT_ON_THE_WIRE`], so a classified-but-unconstructed
/// (or constructed-but-unclassified) variant makes the two counts disagree.
fn every_order_intent_variant_name() -> Vec<&'static str> {
    // Construction is cheap and the values are never used for anything but their discriminant.
    vec![
        order_intent_name(&OrderIntent::Submit(Box::default())),
        order_intent_name(&OrderIntent::SubmitBatch(Vec::new())),
        order_intent_name(&OrderIntent::Cancel(String::new())),
        order_intent_name(&OrderIntent::CancelBatch(Vec::new())),
        order_intent_name(&OrderIntent::Modify {
            order_client_id: String::new(),
            new_qty: None,
            new_price: None,
            new_trigger_price: None,
        }),
        order_intent_name(&OrderIntent::Confirm(String::new())),
        order_intent_name(&OrderIntent::MassCancel { venue: None, symbol: None, account: None }),
        order_intent_name(&OrderIntent::Flatten {
            venue: String::new(),
            symbol: String::new(),
            account: None,
        }),
        order_intent_name(&OrderIntent::MarketExit { venue: None, account: None }),
        order_intent_name(&OrderIntent::Bracket(Box::new(OrderBracket {
            venue: String::new(),
            symbol: String::new(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 0.0,
            take_profit: 0.0,
            account: None,
            order_client_id: None,
        }))),
        order_intent_name(&OrderIntent::ArmConditional(ConditionalIntent {
            venue: String::new(),
            symbol: String::new(),
            side: 1,
            qty: 1.0,
            price: None,
            trail: None,
            trigger_by: None,
            kind_terms: None,
            account: None,
        })),
        order_intent_name(&OrderIntent::DisarmConditional { arm_id: String::new() }),
        order_intent_name(&OrderIntent::Combo(Box::new(OrderCombo {
            venue: String::new(),
            side: 1,
            qty: 1.0,
            legs: vec![
                OrderComboLeg { symbol: "leg-a".to_string(), ratio: 1, limit: None },
                OrderComboLeg { symbol: "leg-b".to_string(), ratio: -1, limit: None },
            ],
            net_limit: None,
            time_in_force: Default::default(),
            account: None,
            order_client_id: None,
        }))),
        order_intent_name(&OrderIntent::OrderList(Box::new(OrderList {
            venue: String::new(),
            symbol: String::new(),
            legs: Vec::new(),
            account: None,
        }))),
        order_intent_name(&OrderIntent::Exercise(Box::new(ExerciseSpec {
            venue: String::new(),
            symbol: String::new(),
            qty: 1.0,
            account: None,
        }))),
    ]
}

/// Every [`Command`] variant name, the [`every_order_intent_variant_name`] twin. `Command::Order`
/// is deliberately NOT constructed here — it delegates to [`order_intent_name`] (see
/// [`command_name`]) and every name it could produce is already covered above.
fn every_command_variant_name() -> Vec<&'static str> {
    vec![
        command_name(&Command::OrderOn(Box::new(RoutedOrder {
            route_key: String::new(),
            intent: OrderIntent::Cancel(String::new()),
        }))),
        command_name(&Command::Emulated(Box::new(EmulatedStep {
            parent: String::new(),
            mount_id: None,
            verb: EmulatedVerb::End(EmulatedEnd::Filled),
        }))),
        command_name(&Command::UpdateParams(Box::new(ParamsUpdate {
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            mount_id: None,
            params: StrategyParams::SpreadMaker(SpreadMakerParams {
                qty: 1.0,
                half_spread: 0.5,
                target_inventory: 0.0,
                max_inventory: 1.0,
                skew: 0.0,
                fill_window_ms: 0,
                net_fill_threshold: 0.0,
                suppress_cooldown_ms: 0,
                style: QuoteStyle::Mid,
                depth_levels: 1,
                tick_size: 0.0,
                filter_own: false,
                avellaneda_stoikov: None,
                refresh_tolerance: None,
                ladder: None,
                reward: None,
                toxicity: None,
            }),
        }))),
        command_name(&Command::SetTradingState(TradingState::Active)),
        command_name(&Command::ResumeVenue { venue: String::new(), force: false }),
        command_name(&Command::SetAccountTradingState {
            route_key: String::new(),
            state: TradingState::Active,
        }),
        command_name(&Command::HoldAccount { route_key: String::new(), hold: true }),
        command_name(&Command::SetMargin(Box::new(MarginUpdate {
            venue: String::new(),
            symbol: String::new(),
            initial_margin_requirement: 0.0,
        }))),
        command_name(&Command::AdmitSymbol(Box::new(SymbolAdmission {
            route_key: String::new(),
            symbol: String::new(),
            grid: SymbolGrid::default(),
            contract_value: None,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: None,
            quote_rate: None,
            rate_as_of_ms: None,
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        }))),
        command_name(&Command::ReconcileReports(Box::new(ReconcileReports {
            venue: String::new(),
            since: 0,
            orders: Vec::new(),
            fills: Vec::new(),
            positions: Vec::new(),
            policy: ReconcilePolicy::hybrid(),
            balance: None,
            generate_missing_orders: false,
            reconcile_balance: false,
            balance_tol: ReconcileBalanceTolerance::default(),
            route_key: None,
            admitted_symbols: None,
            lists_trigger_orders: false,
        }))),
        command_name(&Command::ConfirmReconcile(0)),
        command_name(&Command::CancelReconcileOrder(0)),
        command_name(&Command::MountStrategy(Box::new(MountSpec {
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            account: None,
            controller_id: None,
            file: None,
            name: None,
            rhai: None,
            params: vike_model::params::Params::new(),
            order_client_id_code: None,
        }))),
        command_name(&Command::UnmountStrategy { controller_id: String::new() }),
        command_name(&Command::UnlatchMount { mount_id: String::new() }),
        command_name(&Command::Shutdown),
    ]
}

#[test]
fn no_excused_row_is_also_mapped() {
    for (name, _) in NOT_ON_THE_WIRE {
        assert!(
            !ON_THE_WIRE.iter().any(|(core_name, _)| core_name == name),
            "{name} is in BOTH tables - a mapped capability may not also carry an excuse"
        );
    }
}

/// **Closes the hole `every_order_intent_variant_name`'s doc now names explicitly**: the exhaustive
/// matches force a new match arm on a new variant, but nothing forces that arm's construction into
/// [`every_order_intent_variant_name`]/[`every_command_variant_name`], so an author can satisfy the
/// compiler and still leave a classified variant permanently unconstructed (or a constructed
/// variant unclassified). Either way the two counts below stop agreeing.
#[test]
fn every_constructed_variant_is_counted_exactly_once() {
    let constructed = every_order_intent_variant_name().len() + every_command_variant_name().len();
    let classified = ON_THE_WIRE.len() + NOT_ON_THE_WIRE.len();
    assert_eq!(
        constructed, classified,
        "the constructed variant count ({constructed}) and the classified row count \
         ({classified}) disagree - a variant was added to an exhaustive match but never added to \
         its builder function (every_order_intent_variant_name / every_command_variant_name), or a \
         table row was added/duplicated with no matching construction. Every real OrderIntent/\
         Command variant must be constructed exactly once and classified exactly once."
    );
}

#[test]
fn every_excuse_states_a_reason() {
    for (name, reason) in NOT_ON_THE_WIRE {
        assert!(
            reason.len() > 40,
            "{name}'s reason is too short to be one: {reason:?}. A row is a written admission."
        );
    }
}
