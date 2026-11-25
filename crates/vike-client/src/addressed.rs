//! **Which engine a command reaches, and what the NODE's own frame says of it**: the `tier` an MCP
//! preview shows and the contract value the guardrail sizes an order by, off ONE resolution, so the
//! two cannot name different accounts. The word is always the node's, never an argument's and never
//! a local row's; what the frame cannot answer is an `Err` saying why, and each surface shows that
//! instead of a guess.
//!
//! # The address is the door's
//!
//! A `Submit` or a `Bracket` of the default account on a venue with an up lane twin is addressed as
//! `crates/vike-tradehub/src/server/symbol_admission.rs`'s `lane_addressed` addresses it: a perpetual
//! goes to the twin's engine, named `DEFAULT` or not. Everything else names its book outright, and an
//! order named by id is looked up in the frame's `orders`. The block is the one under that route key,
//! the key `crates/vike-tradehub/src/server/control.rs`'s `label_value` looks the value up by.
//!
//! # The value is the frame's grid, and a missing row is not always the default
//!
//! The node sizes an order at `vike_execution::VenueBlock::contract_value_of`, and the frame carries that
//! grid's non-default rows (`VenueBlockWire::contract_values`, decision 0170). The grid is SPARSE, so
//! no row reads as `Linear` 1.0 only for a symbol the engine says it trades. Any other symbol's value
//! is what the node's door reads from the symbol's own facts when the order arrives (`foreign_contract_value`
//! in the same file as `lane_addressed`): an IG epic or a future with a multiplier, or a pair booked
//! through a conversion rate, has none in the frame. So a symbol the engine does not trade yet, and an
//! engine that does not say what it trades (`symbols` empty: every Polymarket account, an older node),
//! is an `Err`, because `Linear` 1.0 there could UNDER-value the order and read as within a ceiling it breaches.

use std::borrow::Cow;

use vike_model::accounts::account_keys::{
    AccountLabel, RESERVED_DEFAULT_LABEL, label_of_route_key, parse_wire_account, route_key_of,
};
use vike_model::{AccountTier, ContractValue, NotionalKind};
use vike_tradehub_client::wire::{
    CommandWire, CoreSnapshotWire, OrderComboWire, OrderViewWire, VenueBlockWire, block_key,
    names_no_account_refusal,
};

use crate::health::is_lane_twin_block;
use crate::verbs::{Guardrail, GuardrailCapabilities, guardrail_check, guardrail_of_size};

/// How a command names its book. Decided from the command alone, so a verb that names none says so
/// whether or not a frame could be read.
enum Address<'a> {
    /// The node-wide verbs, a venue-less fan-out, and what is no order and no book.
    NoBook,
    /// An order or a bracket: the door may move the default account's onto its lane's engine.
    Order { venue: &'a str, account: Option<&'a str>, symbol: &'a str },
    /// A book named as it stands.
    Book { venue: &'a str, account: Option<&'a str>, symbol: Option<&'a str> },
    /// An order named by id: its row in the frame says whose it is.
    Resting(&'a str),
}

fn address_of(cmd: &CommandWire) -> Address<'_> {
    match cmd {
        CommandWire::Submit(o) => {
            Address::Order { venue: &o.venue, account: o.account.as_deref(), symbol: &o.symbol }
        }
        CommandWire::Bracket(b) => {
            Address::Order { venue: &b.venue, account: b.account.as_deref(), symbol: &b.symbol }
        }
        CommandWire::OrderList(l) => {
            Address::Order { venue: &l.venue, account: l.account.as_deref(), symbol: &l.symbol }
        }
        // A combo names its book and no one symbol: each leg is its own instrument.
        CommandWire::Combo(c) => {
            Address::Book { venue: &c.venue, account: c.account.as_deref(), symbol: None }
        }
        CommandWire::Cancel(order_client_id) | CommandWire::Modify { order_client_id, .. } => {
            Address::Resting(order_client_id)
        }
        CommandWire::MountStrategy { venue, account, symbol, .. }
        | CommandWire::Flatten { venue, symbol, account } => {
            Address::Book { venue, account: account.as_deref(), symbol: Some(symbol) }
        }
        CommandWire::MassCancel { venue: Some(venue), account, .. }
        | CommandWire::MarketExit { venue: Some(venue), account } => {
            Address::Book { venue, account: account.as_deref(), symbol: None }
        }
        CommandWire::SetAccountTradingState { venue, account, .. } => {
            Address::Book { venue, account: account.as_deref(), symbol: None }
        }
        CommandWire::UnadmitSymbol { venue, account, symbol } => {
            Address::Book { venue, account: account.as_deref(), symbol: Some(symbol) }
        }
        // NO WILDCARD: a new verb must be classified here, as
        // `crates/vike-tradehub-client/src/wire.rs`'s `addressed_venue` asks.
        CommandWire::MassCancel { venue: None, .. }
        | CommandWire::MarketExit { venue: None, .. }
        | CommandWire::SetTradingState(_)
        | CommandWire::UpdateParams { .. }
        | CommandWire::UnmountStrategy { .. }
        | CommandWire::ReconcileConfirm { .. }
        | CommandWire::ReconcileCancel { .. }
        | CommandWire::ResumeVenue { .. }
        | CommandWire::UnlatchMount { .. }
        | CommandWire::SetSetting { .. } => Address::NoBook,
    }
}

/// The route key of `account` of `venue`: absent and `DEFAULT` both name the bare venue.
fn book_key(venue: &str, account: Option<&str>) -> Result<String, String> {
    let label = match account {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text)
            .map_err(|e| format!("account {text:?} is not a legal account name: {e}"))?,
    };
    Ok(route_key_of(venue, &label))
}

/// The route key the door addresses an order to (this module's doc). The twin is told by its shape, as
/// `crate::health`'s `collect_accounts` tells it. ⚠ At `live` the door also asks the account watcher, whose word is
/// not on the frame, so a real account of the twin's shape reads as the twin here and the door decides.
fn order_key(
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    frame: &CoreSnapshotWire,
) -> Result<String, String> {
    if let Some(twin) = vike_venues::lane_twin(venue)
        && matches!(account, None | Some(RESERVED_DEFAULT_LABEL))
        && vike_catalog::on_futures_lane(venue, symbol)
        && frame.venues.iter().any(|block| is_lane_twin_block(twin, block))
    {
        return Ok(twin.route_key());
    }
    book_key(venue, account)
}

/// **The account a bare venue means, as the door resolves it** (decision 0254): `account` itself when one is named,
/// the one account of the venue whatever its label, and `Err` naming the books where the venue has several, running
/// or not, so a preview never shows a tier beside a command the door refuses. A frame the door would have to answer
/// for (no account of the venue on it) is left to the lookup that follows.
fn named_or_sole<'a>(
    venue: &str,
    account: Option<&'a str>,
    frame: &CoreSnapshotWire,
) -> Result<Option<Cow<'a, str>>, String> {
    if let Some(account) = account {
        return Ok(Some(Cow::Borrowed(account)));
    }
    let keys = crate::verbs::venue_keys_on_frame(venue, frame);
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    match crate::verbs::accounts_of_venue(venue, &keys).as_slice() {
        [only] => Ok(label_of_route_key(venue, only)
            .filter(|label| !label.is_default())
            .map(|label| Cow::Owned(label.to_string()))),
        [] => Ok(None),
        _ => Err(names_no_account_refusal("this command", venue, &keys)),
    }
}

/// The block `address` reaches and the symbol it names on it, or why the frame cannot say. A NOT
/// RUNNING account's retained block is no engine an order reaches (`crate::health`'s `running`).
fn engine_of<'a>(
    address: Address<'a>,
    frame: &'a CoreSnapshotWire,
) -> Result<(&'a VenueBlockWire, Option<&'a str>), String> {
    let (key, symbol) = match address {
        Address::NoBook => return Err("this command names no one account".to_string()),
        Address::Order { venue, account, symbol } => {
            let account = named_or_sole(venue, account, frame)?;
            (order_key(venue, account.as_deref(), symbol, frame)?, Some(symbol))
        }
        Address::Book { venue, account, symbol } => {
            let account = named_or_sole(venue, account, frame)?;
            (book_key(venue, account.as_deref())?, symbol)
        }
        Address::Resting(order_client_id) => {
            let order =
                frame.orders.iter().find(|o| o.order_client_id == order_client_id).ok_or_else(|| {
                    format!(
                        "the node's frame holds no order `{order_client_id}` (placed moments ago, or it never \
                     existed), so which account it rests in is not known"
                    )
                })?;
            (book_key(&order.venue, order.account.as_deref())?, Some(order.symbol.as_str()))
        }
    };
    if let Some(stopped) = frame.stopped_row(&key) {
        return Err(format!(
            "the node lists `{key}` NOT RUNNING ({}), so no order reaches it",
            stopped.say()
        ));
    }
    if frame.venues.is_empty() {
        return Err("the node has published no engine yet (it has not folded)".to_string());
    }
    // The core's list only grows, so a later block under one key is the newer engine's.
    let block = frame
        .venues
        .iter()
        .rev()
        .find(|block| block_key(block) == key)
        .ok_or_else(|| format!("the node's frame carries no engine under `{key}`"))?;
    Ok((block, symbol))
}

/// The block `cmd` reaches on `frame` and the signed net position it holds in the symbol `cmd` names (`0` for a book
/// named with no one symbol, a combo's): what `crate::verbs::fill_order_client_id` takes an id's shape, account code
/// and intent from. `None` where this module's resolution cannot say.
pub(crate) fn order_client_id_target<'a>(
    cmd: &'a CommandWire,
    frame: &'a CoreSnapshotWire,
) -> Option<(&'a VenueBlockWire, f64)> {
    let (block, symbol) = engine_of(address_of(cmd), frame).ok()?;
    Some((block, symbol.map_or(0.0, |symbol| net_position(block, symbol))))
}

/// **The tier of the account `cmd` reaches, as the node names it.** `None` for a verb that names no
/// one account, which says nothing rather than something false; `Some(Err)` when it names one and
/// `frame` (or why there is none) cannot say which tier that is: a tier word the parser refuses reads
/// the same as an absent one (`VenueBlockWire::tier`'s doc), and neither is ever guessed.
pub fn tier_of(
    cmd: &CommandWire,
    frame: Result<&CoreSnapshotWire, &str>,
) -> Option<Result<AccountTier, String>> {
    let address = address_of(cmd);
    if matches!(address, Address::NoBook) {
        return None;
    }
    Some(frame.map_err(str::to_string).and_then(|frame| {
        let (block, _) = engine_of(address, frame)?;
        let key = block_key(block);
        let word = block.tier.as_deref().ok_or_else(|| {
            format!("the node does not name the tier of `{key}` (it predates the field)")
        })?;
        word.parse::<AccountTier>()
            .map_err(|e| format!("the node's word for the tier of `{key}` is no tier: {e}"))
    }))
}

/// **What one unit of the quantity `cmd` states is worth**, for [`crate::verbs::guardrail_check`]: the
/// row of the engine it reaches, the default for a symbol that engine says it trades and no row names,
/// and `Err` saying why for anything else (this module's doc). A row a client cannot read is an `Err`
/// too: the door refuses a unit that is no finite positive number rather than compare it.
pub fn contract_value_of(
    cmd: &CommandWire,
    frame: Result<&CoreSnapshotWire, &str>,
) -> Result<ContractValue, String> {
    if matches!(cmd, CommandWire::Combo(_)) {
        return Err(
            "a combo's legs are each sized on their own instrument, by the node's door".to_string()
        );
    }
    let (block, symbol) = engine_of(address_of(cmd), frame.map_err(str::to_string)?)?;
    let symbol = symbol.ok_or("this command names no symbol")?;
    value_on(block, symbol)
}

/// What one unit of `symbol` is worth on `block`, [`contract_value_of`]'s rule (this module's doc).
fn value_on(block: &VenueBlockWire, symbol: &str) -> Result<ContractValue, String> {
    let key = block_key(block);
    if let Some(row) = block.contract_values.iter().find(|row| row.symbol == symbol) {
        let Some(kind) = NotionalKind::from_word(&row.kind) else {
            return Err(format!("the node's row for `{symbol}` names a kind {:?}", row.kind));
        };
        if !(row.per_unit.is_finite() && row.per_unit > 0.0) {
            return Err(format!("the node's row for `{symbol}` names a unit {}", row.per_unit));
        }
        return Ok(ContractValue { kind, per_unit: row.per_unit });
    }
    if block.symbols.is_empty() {
        return Err(format!(
            "the node does not say what `{key}` trades, so the value of `{symbol}` is not known"
        ));
    }
    if block.symbols.iter().any(|traded| traded == symbol) {
        return Ok(ContractValue::default());
    }
    Err(format!(
        "`{key}` does not trade `{symbol}` yet, so the node's frame carries no value for it"
    ))
}

/// The signed net position the block holds in the symbol an order names (long positive), any case: a blank symbol is
/// the engine's own. The door's own reading is `crates/vike-tradehub/src/server/refusal.rs`'s `net_position`.
fn net_position(block: &VenueBlockWire, symbol: &str) -> f64 {
    use vike_model::events::PositionSide;
    let typed = symbol.trim();
    let named =
        if typed.is_empty() { block.symbols.first().map_or("", String::as_str) } else { typed };
    block
        .positions
        .iter()
        .filter(|p| p.symbol.eq_ignore_ascii_case(named))
        .map(|p| PositionSide::from_word_or_both(&p.position_side).signed_size(p.size))
        .sum()
}

/// **Whether the door takes `cmd` for a CLOSE, which it never sizes**: `crates/vike-tradehub/src/server/symbol_admission.rs`'s
/// `order_closes` and `modify_closes`, off what the frame shows, held to them by that crate's table
/// (`crates/vike-tradehub/src/server/tests/client_door.rs`). The position decides, on any venue; the flag alone decides
/// only where the venue enforces it. An order the frame cannot place reads as no close, and is sized.
///
/// An ORDER LIST is a close when every leg is reduce-only and the position covers each (an exit OCO on a covered
/// long: D4 of 2026-11-24, the door's `order_list_closes`). A MODIFY stays a close while the position covers the
/// size it leaves, the resting size when it names none.
fn closes(cmd: &CommandWire, frame: Result<&CoreSnapshotWire, &str>) -> bool {
    let covers = |frame: &CoreSnapshotWire, side, symbol: &str, qty| {
        engine_of(address_of(cmd), frame).is_ok_and(|(block, _)| {
            vike_model::is_covered_reduce(side, net_position(block, symbol), qty)
        })
    };
    match cmd {
        CommandWire::Submit(o) => {
            frame.is_ok_and(|frame| covers(frame, o.side, &o.symbol, o.qty))
                || (o.reduce_only && vike_venues::capabilities_for(&o.venue).supports_reduce_only)
        }
        CommandWire::OrderList(l) => {
            !l.legs.is_empty()
                && frame.is_ok_and(|frame| {
                    l.legs
                        .iter()
                        .all(|leg| leg.reduce_only && covers(frame, leg.side, &l.symbol, leg.qty))
                })
        }
        CommandWire::Modify { order_client_id, new_qty, .. } => frame.is_ok_and(|frame| {
            open_order(frame, order_client_id)
                .is_some_and(|o| covers(frame, o.side, &o.symbol, new_qty.unwrap_or(o.qty)))
        }),
        _ => false,
    }
}

/// The OPEN order `order_client_id` names on `frame` (a terminal one is an order the core ignores a modify of).
fn open_order<'a>(frame: &'a CoreSnapshotWire, order_client_id: &str) -> Option<&'a OrderViewWire> {
    frame.orders.iter().find(|o| {
        o.order_client_id == order_client_id
            && vike_tradehub_client::observe_decode::order_status_of(&o.status)
                .is_some_and(|status| !status.is_terminal())
    })
}

/// **A modify measured from the resting order it names** (D3 of 2026-11-24): a missing qty is the resting qty and a
/// missing price the resting order's own price (its limit, else its trigger; a resting MARKET order has none), so a
/// price-only modify cannot raise a notional unmeasured. `None` for a modify that names both, any other command,
/// and an order the frame does not hold (placed moments ago, or never): that one is left as named.
fn with_resting_terms(
    cmd: &CommandWire,
    frame: Result<&CoreSnapshotWire, &str>,
) -> Option<CommandWire> {
    let CommandWire::Modify { order_client_id, new_qty, new_price, new_trigger_price } = cmd else {
        return None;
    };
    let names_a_price = new_price.is_some() || new_trigger_price.is_some();
    if new_qty.is_some() && names_a_price {
        return None;
    }
    let resting = open_order(frame.ok()?, order_client_id)?;
    let market = resting.order_type.trim().eq_ignore_ascii_case("market");
    let (price, trigger) = match (names_a_price, market) {
        (true, _) => (*new_price, *new_trigger_price),
        (false, true) => (None, None),
        (false, false) => (resting.price, resting.trigger_price),
    };
    Some(CommandWire::Modify {
        order_client_id: order_client_id.clone(),
        new_qty: Some(new_qty.unwrap_or(resting.qty)),
        new_price: price,
        new_trigger_price: trigger,
    })
}

/// **A combo priced by its legs, measured as the node's door measures it** (D2 of 2026-11-24): each leg is
/// `|ratio| × qty` of its own instrument at its own limit, valued on the combo's engine, and the largest leg is the one
/// that can breach (each leg is one order, so not a sum). A combo priced by its net, or at market, names no price per
/// leg (its signed net is no size), and a leg the frame carries no value for leaves it unpriced: `Err` says why.
fn largest_combo_leg(
    cmd: &CommandWire,
    combo: &OrderComboWire,
    frame: Result<&CoreSnapshotWire, &str>,
) -> Result<(f64, f64, ContractValue), String> {
    if combo.legs.iter().any(|leg| leg.limit.is_none()) {
        return Err("a combo priced by its net, or at market, names no price per leg; the node's door sizes its \
                    legs at the prices it holds"
            .to_string());
    }
    let (block, _) = engine_of(address_of(cmd), frame.map_err(str::to_string)?)?;
    let mut largest: Option<(f64, f64, ContractValue)> = None;
    for leg in &combo.legs {
        let Some(limit) = leg.limit else { continue };
        let value = value_on(block, &leg.symbol)?;
        let qty = combo.qty * f64::from(leg.ratio.unsigned_abs());
        if largest.is_none_or(|(q, p, v)| value.order_notional(qty, limit) > v.order_notional(q, p))
        {
            largest = Some((qty, limit, value));
        }
    }
    largest.ok_or_else(|| "a combo with no leg has nothing to size".to_string())
}

/// **The guardrail of `cmd` as the node's `frame` shows it**: what its quantity is worth and whether the door would
/// size it at all. What the frame cannot answer is the line's to say.
pub fn guardrail_of(
    cmd: &CommandWire,
    capabilities: GuardrailCapabilities,
    frame: Result<&CoreSnapshotWire, &str>,
) -> Guardrail {
    if let CommandWire::Combo(combo) = cmd {
        return match largest_combo_leg(cmd, combo, frame) {
            Ok((qty, price, value)) => {
                guardrail_of_size(qty, Some(price), false, capabilities, Ok(value))
            }
            Err(why) => guardrail_check(cmd, capabilities, Err(why), false),
        };
    }
    let filled = with_resting_terms(cmd, frame);
    let cmd = filled.as_ref().unwrap_or(cmd);
    guardrail_check(cmd, capabilities, contract_value_of(cmd, frame), closes(cmd, frame))
}

#[path = "addressed_tests.rs"]
#[cfg(test)]
mod addressed_tests;
