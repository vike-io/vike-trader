//! **A preview answers what the send will answer.** MCP calls a preview's verdict "the verdict that counts", and it
//! was not: `accept_command` and the two Previews each spelled the door's steps by hand, and a step that landed in one
//! kept the others accepting. The table runs every `CommandWire` variant over the shapes of `one_account_door.rs`,
//! then the shapes on which the copies differed, and asks the send and the preview for one answer.
//!
//! What a preview cannot say stays out, each for its own reason: a settings write (no source here, and the loader
//! judges the value), an order frame `lower_command` refuses as malformed (its order terms aside), and what only a
//! commit learns (the first price of a held order, a due refresh, a leverage the exchange confirms).

use super::one_account_door::{SHAPES, frames, one_engine_frames};
use super::*;
use crate::publish::{named_mounts, named_orders, running_route_keys};
use crate::server::control::ControlAcceptError;
use crate::server::preview::{DoorReads, preview_verdict};
use vike_execution::{MountBudget, MountView, MountViewKind, OrderView, VenueBlock};
use vike_hub_protocol::refusal::HubRefusal;
use vike_model::{OrderStatus, OrderType, TradingState};
use vike_tradehub_client::wire::OrderBracketWire;

/// One node as the door reads it: what the send is handed and what a preview reads.
struct Door {
    blocks: Vec<VenueBlock>,
    not_running: Vec<vike_execution::ArmingNotRunningView>,
    orders: Vec<std::sync::Arc<OrderView>>,
    mounts: Vec<MountView>,
    max_notional: Option<f64>,
}

impl Door {
    fn of(blocks: Vec<VenueBlock>) -> Self {
        Door {
            blocks,
            not_running: Vec::new(),
            orders: Vec::new(),
            mounts: Vec::new(),
            max_notional: None,
        }
    }

    fn capped(mut self, max: f64) -> Self {
        self.max_notional = Some(max);
        self
    }

    fn limits(&self) -> ControlLimits {
        ControlLimits::new(ControlLimitsConfig {
            max_notional: self.max_notional,
            rate_per_sec: 1e9,
        })
    }

    fn engines(&self) -> Vec<String> {
        self.blocks.iter().map(|b| b.venue.clone()).collect()
    }

    fn route_keys(&self) -> Vec<String> {
        running_route_keys(&self.blocks, &self.not_running)
    }
}

/// [`Door`] and its facts as a Preview reads them.
struct Reads<'a> {
    door: &'a Door,
    facts: &'a dyn SymbolAdmitter,
}

impl DoorReads for Reads<'_> {
    fn engine_venues(&self) -> Vec<String> {
        self.door.engines()
    }
    fn engine_route_keys(&self) -> Vec<String> {
        self.door.route_keys()
    }
    fn engine_blocks(&self) -> Vec<VenueBlock> {
        self.door.blocks.clone()
    }
    fn not_running(&self) -> Arc<Vec<vike_execution::ArmingNotRunningView>> {
        Arc::new(self.door.not_running.clone())
    }
    fn orders_named_by(&self, cmd: &CommandWire) -> Vec<OrderView> {
        named_orders(cmd, &self.door.orders)
    }
    fn mounts_named_by(&self, cmd: &CommandWire) -> Vec<MountView> {
        named_mounts(cmd, &self.door.mounts)
    }
    fn symbol_admitter(&self) -> Option<&dyn SymbolAdmitter> {
        Some(self.facts)
    }
}

fn previewed(door: &Door, cmd: &CommandWire, facts: &dyn SymbolAdmitter) -> Option<HubRefusal> {
    preview_verdict(cmd, &Reads { door, facts }, &mut door.limits()).refusal
}

/// The send's answer, through the ONE acceptance path into a real core's lane. Its reads are the door's own, so the
/// core behind the lane decides nothing here: it only takes what is accepted. A refusal is its frame: its id and its
/// text, both of which the preview must give.
fn send(
    door: &Door,
    cmd: &CommandWire,
    facts: &dyn SymbolAdmitter,
    sink: &vike_core::CommandSink,
) -> Option<HubRefusal> {
    let mut limits = door.limits();
    let orders = named_orders(cmd, &door.orders);
    let mounts = named_mounts(cmd, &door.mounts);
    loop {
        let answer = accept_command(
            cmd.clone(),
            None,
            &mut limits,
            sink,
            None,
            &door.engines(),
            &door.route_keys(),
            &door.not_running,
            &door.blocks,
            &orders,
            &[],
            &mounts,
            Some(facts),
            None,
            None,
        );
        match answer {
            Ok(_) => return None,
            // The lane is the test's real core catching up: the answer it owes is the next one.
            Err(ControlAcceptError::Busy) => std::thread::sleep(Duration::from_millis(1)),
            Err(refused) => return Some(refused.refusal()),
        }
    }
}

/// Every wire verb a tool or a grammar can name on `venue`, one frame each and the account-bearing ones three ways.
/// The match below has no wildcard, so a new variant does not compile until it has a row here.
fn every_verb(venue: &str) -> Vec<(String, CommandWire)> {
    let mut verbs: Vec<(String, CommandWire)> = Vec::new();
    for account in [None, Some("DEFAULT"), Some("LIVE")] {
        for (subject, cmd) in frames(venue, account) {
            verbs.push((format!("{subject} {account:?}"), cmd));
        }
    }
    for (subject, cmd) in one_engine_frames(venue) {
        verbs.push((subject.to_string(), cmd));
    }
    let retune = crate::server::tests::lower_command::spread_maker_params(1.0, 0.5);
    let plain = [
        CommandWire::Cancel("c-1".into()),
        CommandWire::Modify {
            order_client_id: "c-1".into(),
            new_qty: Some(1.0),
            new_price: Some(1.0),
            new_trigger_price: None,
        },
        CommandWire::MassCancel { venue: None, symbol: None, account: None },
        CommandWire::MarketExit { venue: None, account: None },
        CommandWire::SetTradingState(TradingState::Reducing),
        CommandWire::UpdateParams {
            venue: venue.to_string(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            mount_id: None,
            params: serde_json::to_value(retune).expect("the core's own params serialize"),
        },
        CommandWire::UnmountStrategy { controller_id: "m_a".into() },
        CommandWire::ReconcileConfirm { id: 1 },
        CommandWire::ReconcileCancel { id: 1 },
        CommandWire::ResumeVenue { venue: venue.to_string(), force: false },
        CommandWire::UnlatchMount { mount_id: "m_a".into() },
    ];
    verbs.extend(plain.into_iter().map(|cmd| (verb_name(&cmd).to_string(), cmd)));
    verbs.push(("the market order".to_string(), market(venue, "BTCUSDT", false)));
    verbs
}

fn verb_name(cmd: &CommandWire) -> &'static str {
    match cmd {
        CommandWire::Submit(_) => "submit",
        CommandWire::Bracket(_) => "bracket",
        CommandWire::OrderList(_) => "order_list",
        CommandWire::Combo(_) => "combo",
        CommandWire::Cancel(_) => "cancel",
        CommandWire::Modify { .. } => "modify",
        CommandWire::MassCancel { .. } => "mass_cancel",
        CommandWire::Flatten { .. } => "flatten",
        CommandWire::MarketExit { .. } => "market_exit",
        CommandWire::SetTradingState(_) => "set_trading_state",
        CommandWire::UpdateParams { .. } => "update_params",
        CommandWire::MountStrategy { .. } => "mount",
        CommandWire::UnmountStrategy { .. } => "unmount",
        CommandWire::SetSetting { .. } => "set_setting",
        CommandWire::ReconcileConfirm { .. } => "recon_confirm",
        CommandWire::ReconcileCancel { .. } => "recon_cancel",
        CommandWire::ResumeVenue { .. } => "resume_venue",
        CommandWire::SetAccountTradingState { .. } => "set_account_trading_state",
        CommandWire::UnadmitSymbol { .. } => "unadmit_symbol",
        CommandWire::UnlatchMount { .. } => "unlatch_mount",
    }
}

fn stopped(venue: &str, route_key: &str) -> vike_execution::ArmingNotRunningView {
    vike_execution::ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::from_route_key(venue, route_key),
        tier: Some(AccountTier::Demo),
        reason: "account inactive".to_string(),
        block: Some(vike_model::ArmingBlock::AccountInactive),
        refusal_id: None,
    }
}

fn in_state(mut block: VenueBlock, state: TradingState) -> VenueBlock {
    block.trading_state = state;
    block
}

fn holding(mut block: VenueBlock, symbol: &str, size: f64) -> VenueBlock {
    block.positions.push(vike_execution::PositionView {
        venue: block.venue.as_str().into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
        size,
        average_price: 3_000.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liquidation_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    });
    block
}

fn admitted(mut block: VenueBlock, symbol: &str) -> VenueBlock {
    block.extra_symbols = vec![symbol.to_string()].into();
    block
}

fn order_on(order_client_id: &str, venue: &str, symbol: &str, status: OrderStatus) -> OrderView {
    OrderView {
        data_delay: None,
        order_client_id: order_client_id.to_string(),
        venue: venue.to_string(),
        account: None,
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

fn mount_row(id: &str, latched: bool) -> MountView {
    MountView {
        kind: MountViewKind::Mount,
        mount_id: id.into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        ready: true,
        position: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        notional: 0.0,
        budget: Some(MountBudget {
            max_loss: Some(50.0),
            max_notional: None,
            loss_action: vike_model::MountLossAction::Flatten,
            max_leverage: None,
        }),
        latched,
        params: None,
        currency: Default::default(),
        legs: None,
    }
}

fn plain_sell(venue: &str, symbol: &str, qty: f64, price: f64) -> CommandWire {
    let CommandWire::Submit(mut req) = limit("c-close", venue, symbol, qty, price) else {
        unreachable!()
    };
    req.side = -1;
    CommandWire::Submit(req)
}

fn market_bracket(venue: &str, symbol: &str, qty: f64) -> CommandWire {
    CommandWire::Bracket(OrderBracketWire {
        order_client_id: None,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
    })
}

/// One row of the table: the node, the command, what the node holds a price at, and for the rows written by hand what
/// the send must say (a word of its refusal, or nothing), so the table cannot pass on two wrong answers.
struct Scene {
    name: String,
    door: Door,
    command: CommandWire,
    facts: Table,
    says: Option<Option<&'static str>>,
}

fn scene(
    name: &str,
    says: Option<&'static str>,
    door: Door,
    cmd: CommandWire,
    facts: Table,
) -> Scene {
    Scene { name: name.to_string(), door, command: cmd, facts, says: Some(says) }
}

/// The shapes on which the old previews and the send named different answers, and what must stay as it is beside them.
fn scenes() -> Vec<Scene> {
    let bybit = || engine("bybit", "BTCUSDT");
    let halted = || in_state(bybit(), TradingState::Halted);
    let new_symbol = || limit("c-new", "bybit", "ETHUSDT", 0.01, 3_000.0);
    let give_back = |symbol: &str| CommandWire::UnadmitSymbol {
        venue: "bybit".into(),
        account: None,
        symbol: symbol.into(),
    };
    let unlatch = |id: &str| CommandWire::UnlatchMount { mount_id: id.into() };
    let mut live_twin = Door::of(vec![
        vike_execution::VenueBlock {
            tier: Some(AccountTier::Live),
            ..engine("binance", "BTCUSDT")
        },
        vike_execution::VenueBlock {
            tier: Some(AccountTier::Live),
            ..engine("binance#PERP", "BTCUSDT.P")
        },
    ]);
    live_twin.max_notional = Some(550.0);
    let stop_market = {
        let CommandWire::Submit(mut req) = submit("binance", "BTCUSDT.P", Some("PERP")) else {
            unreachable!()
        };
        req.qty = 0.01;
        req.order_type = "stop".into();
        req.price = None;
        req.trigger_price = Some(50_000.0);
        CommandWire::Submit(req)
    };
    let mut stopped_door = Door::of(vec![bybit()]).capped(1_000.0);
    stopped_door.not_running = vec![stopped("bybit", "bybit")];
    let mut reused = Door::of(vec![bybit()]).capped(1_000.0);
    reused.orders = vec![order_on("c-1", "bybit", "BTCUSDT", OrderStatus::Filled).into()];
    let mut resting_in_it = Door::of(vec![admitted(bybit(), "ETHUSDT")]);
    resting_in_it.orders = vec![order_on("c-r", "bybit", "ETHUSDT", OrderStatus::Accepted).into()];
    let mut restarting = Door::of(vec![bybit(), engine("bybit#ALT", "BTCUSDT")]);
    restarting.not_running = vec![vike_execution::ArmingNotRunningView {
        tier: Some(AccountTier::Live),
        reason: "tier changed, restarting at `live`".to_string(),
        block: None,
        ..stopped("bybit", "bybit#ALT")
    }];
    let new_symbol_of_alt = || submit("bybit", "BTCUSDT", Some("ALT"));
    let mut not_latched = Door::of(vec![bybit()]);
    not_latched.mounts = vec![mount_row("m_a", false)];
    let mut latched = Door::of(vec![bybit()]);
    latched.mounts = vec![mount_row("m_a", true)];
    let trigger_less = |kind: &str, price: Option<f64>| {
        let CommandWire::Submit(mut req) = limit("c-t", "bybit", "BTCUSDT", 0.01, 1.0) else {
            unreachable!()
        };
        (req.order_type, req.price, req.trigger_price) = (kind.into(), price, None);
        CommandWire::Submit(req)
    };
    vec![
        // A trigger kind with no trigger: the send's lowering refuses it, and the dry run says the same words.
        scene(
            "a stop with no trigger",
            Some("submit: order terms — a stop order needs a trigger price above zero"),
            Door::of(vec![bybit()]),
            trigger_less("stop", None),
            Table::eth(),
        ),
        scene(
            "a stop-limit with no trigger",
            Some("submit: order terms — a stop_limit order needs a trigger price above zero"),
            Door::of(vec![bybit()]),
            trigger_less("stop_limit", Some(3_000.0)),
            Table::eth(),
        ),
        // A new symbol's admission commits a leverage to the exchange, which a node not trading must not do.
        scene(
            "a new symbol, Halted",
            Some("HALTED"),
            Door::of(vec![halted()]),
            new_symbol(),
            Table::eth(),
        ),
        scene(
            "a new symbol, Reducing",
            Some("REDUCING"),
            Door::of(vec![in_state(bybit(), TradingState::Reducing)]),
            new_symbol(),
            Table::eth(),
        ),
        // A close is never held up: it goes without the admission.
        scene(
            "a close, Halted",
            None,
            Door::of(vec![holding(halted(), "ETHUSDT", 1.0)]),
            plain_sell("bybit", "ETHUSDT", 0.5, 3_000.0),
            Table::eth(),
        ),
        scene("an order, Active", None, Door::of(vec![bybit()]), new_symbol(), Table::eth()),
        // The give-back: the symbol must be one the engine admitted, with nothing in it.
        scene(
            "a give-back, no slot",
            Some("it has admitted none"),
            Door::of(vec![bybit()]),
            give_back("ETHUSDT"),
            Table::eth(),
        ),
        scene(
            "a give-back, an order",
            Some("live order"),
            resting_in_it,
            give_back("ETHUSDT"),
            Table::eth(),
        ),
        scene(
            "a give-back, flat",
            None,
            Door::of(vec![admitted(bybit(), "ETHUSDT")]),
            give_back("ETHUSDT"),
            Table::eth(),
        ),
        // The un-latch: the mount must be one the node runs, latched and flat.
        scene(
            "an un-latch, not latched",
            Some("is not latched"),
            not_latched,
            unlatch("m_a"),
            Table::eth(),
        ),
        scene("an un-latch, latched", None, latched, unlatch("m_a"), Table::eth()),
        // Two refusals: the send's order of gates names the routing one first, and the ceiling at the market after.
        scene(
            "a market order, stopped",
            Some("does NOT RUN"),
            stopped_door,
            market("bybit", "BTCUSDT", false),
            Table::eth(),
        ),
        scene(
            "a market order, a used id",
            Some("already used"),
            reused,
            market("bybit", "BTCUSDT", false),
            Table::eth(),
        ),
        scene(
            "a market bracket no spot engine holds",
            Some("spot lane"),
            Door::of(vec![engine("binance", "BTCUSDT")]).capped(1_000.0),
            market_bracket("binance", "BTCUSDT", 1.0),
            Table::eth(),
        ),
        // The ceiling at the market itself, after the gates: both say it.
        scene(
            "a market order over the ceiling",
            Some("exceeds the node's"),
            Door::of(vec![bybit()]).capped(1_000.0),
            market("bybit", "BTCUSDT", false),
            Table::eth(),
        ),
        // A tier change lists the account under its NEW tier while the old engine's block stays: it does not run.
        scene(
            "an order, restarting at another tier",
            Some("does NOT RUN"),
            restarting,
            new_symbol_of_alt(),
            Table::eth(),
        ),
        // A stop-market on a live twin is sized at the higher of its trigger and the price the node holds.
        scene(
            "a stop-market on a live twin",
            Some("exceeds the node's"),
            live_twin,
            stop_market,
            Table::eth()
                .deriving(&["binance#PERP"])
                .with_price(Price::At(60_000.0))
                .confirming(5.0),
        ),
    ]
}

/// The whole table, one scene per shape and verb, at no ceiling and at one every priced row meets.
fn table() -> Vec<Scene> {
    let mut rows = scenes();
    for shape in &SHAPES {
        for max in [None, Some(1.0)] {
            for (subject, cmd) in every_verb(shape.venue) {
                let mut door = Door::of(shape.blocks());
                door.max_notional = max;
                rows.push(Scene {
                    name: format!("{} {max:?}: {subject}", shape.venue),
                    door,
                    command: cmd,
                    facts: Table::eth(),
                    says: None,
                });
            }
        }
    }
    rows
}

/// **The send and the Preview give one answer on every row**, and the hand-written rows give the one they name.
///
/// MUTATION PROOF: in `preview_verdict`'s `door_refusal`, drop `plan_unadmit`, `plan_unlatch`, the order-terms step or
/// the not-active step, or move `market_reason` ahead of the routing gates: the rows named for each come back. Take
/// `order_closes` out of the not-active step and `a close, Halted` is refused, which is the one thing this may never do.
#[test]
fn a_preview_answers_what_the_send_answers_on_every_verb_and_shape() {
    let node = Node::start("hyperliquid", "BTC", AccountTier::Demo);
    let sink = node.core.command_sink();
    let rows = table();
    let total = rows.len();
    let mut differ = Vec::new();
    for row in &rows {
        let sent = send(&row.door, &row.command, &row.facts, &sink);
        let cut = |said: &Option<HubRefusal>| {
            said.as_ref().map_or("accepted".to_string(), |s| {
                format!("[{}] {}", s.id, s.text.chars().take(90).collect::<String>())
            })
        };
        if let Some(says) = row.says {
            let as_named = match (says, &sent) {
                (None, None) => true,
                (Some(word), Some(refusal)) => refusal.text.contains(word),
                _ => false,
            };
            if !as_named {
                differ.push(format!("{}: the send said {}, not {says:?}", row.name, cut(&sent)));
            }
        }
        let said = previewed(&row.door, &row.command, &row.facts);
        if said != sent {
            differ.push(format!(
                "{}\n    send: {}\n    wire: {}",
                row.name,
                cut(&sent),
                cut(&said)
            ));
        }
    }
    node.core.shutdown_and_join();
    assert!(differ.is_empty(), "{} of {total} rows differ:\n{}", differ.len(), differ.join("\n"));
}

/// **A frame malformed twice** — a lifetime that is no word AND a stop with no trigger. The send names its lowering's
/// FIRST fault, the lifetime. The dry run does not judge a lifetime (`preview.rs`'s documented scope), so it keeps its
/// no-verdict for a malformed frame rather than name the second fault the send never reaches.
#[test]
fn a_frame_malformed_before_its_order_terms_gets_no_verdict_never_a_different_fault() {
    let node = Node::start("hyperliquid", "BTC", AccountTier::Demo);
    let sink = node.core.command_sink();
    let door = Door::of(vec![engine("bybit", "BTCUSDT")]);
    let facts = Table::eth();
    let CommandWire::Submit(mut req) = limit("c-t", "bybit", "BTCUSDT", 0.01, 1.0) else {
        unreachable!()
    };
    (req.order_type, req.price, req.trigger_price) = ("stop".into(), None, None);
    req.time_in_force = Some("forever".into());
    let both = CommandWire::Submit(req);
    let sent = send(&door, &both, &facts, &sink);
    node.core.shutdown_and_join();
    let sent = sent.expect("the send refuses the frame").text;
    assert!(sent.starts_with("submit: `time_in_force`"), "the first fault: {sent}");
    assert_eq!(previewed(&door, &both, &facts), None, "no verdict, never `order terms`");
}

/// **A lookup costs a preview a rate token, and a mount's strategy file is judged.**
#[test]
fn a_lookup_takes_a_rate_token_and_a_mounts_file_is_judged() {
    let door = Door::of(vec![engine("bybit", "BTCUSDT")]);
    let facts = Table::eth();
    let new_symbol = limit("c-new", "bybit", "ETHUSDT", 0.01, 3_000.0);
    let empty_bucket =
        || ControlLimits::new(ControlLimitsConfig { max_notional: None, rate_per_sec: 0.0 });
    let reads = Reads { door: &door, facts: &facts };
    let wire = preview_verdict(&new_symbol, &reads, &mut empty_bucket());
    assert!(wire.refusal.is_some_and(|why| why.text.contains("rate limited")));
    let own_symbol = limit("c-own", "bybit", "BTCUSDT", 0.01, 3_000.0);
    let wire = preview_verdict(&own_symbol, &reads, &mut empty_bucket());
    assert_eq!(wire.refusal, None, "a symbol the engine trades costs no lookup");

    let CommandWire::MountStrategy { venue, symbol, interval, params, .. } =
        frames("bybit", None).remove(2).1
    else {
        panic!("the third frame is the mount");
    };
    let not_a_file = CommandWire::MountStrategy {
        venue,
        account: None,
        symbol,
        interval,
        controller_id: None,
        file: Some("not_a_script.txt".into()),
        name: None,
        rhai: None,
        params,
    };
    let wire = previewed(&door, &not_a_file, &facts);
    assert!(
        wire.is_some_and(|why| why.text.contains("mount_strategy")
            && why.text.contains("neither a .rs nor a .rhai")),
        "the file rule refuses it"
    );
}

/// A refusal carries no notes, and an order the node would pass carries what it will go out as.
#[test]
fn a_preview_says_notes_only_beside_an_order_it_passes() {
    let facts = Table::eth().with_price(Price::Pending);
    let door = Door::of(vec![engine("bybit", "BTCUSDT")]);
    let reads = Reads { door: &door, facts: &facts };
    let order = market("bybit", "BTCUSDT", false);
    let passed = preview_verdict(&order, &reads, &mut door.limits());
    assert_eq!(passed.refusal, None);
    assert!(passed.notes.iter().any(|note| note.contains("no price is held")), "{passed:?}");
    let halted = Door::of(vec![in_state(engine("bybit", "BTCUSDT"), TradingState::Halted)]);
    let reads = Reads { door: &halted, facts: &facts };
    let new_symbol = limit("c-new", "bybit", "ETHUSDT", 0.01, 3_000.0);
    let refused = preview_verdict(&new_symbol, &reads, &mut halted.limits());
    assert!(refused.refusal.is_some());
    assert!(refused.notes.is_empty(), "{refused:?}");
}
