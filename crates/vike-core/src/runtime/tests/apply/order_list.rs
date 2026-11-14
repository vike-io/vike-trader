//! Order lists (wave 4 of the order kinds): `lower_order_list`'s atomic preflight, the OUO resize a
//! ladder's fills drive through the amend (`apply/contingency.rs`), the double-fill note, a half-filled
//! ladder across a restart, and the journal's format step.

use super::*;
use vike_model::events::{OrderModified, OrderPartiallyFilled};
use vike_model::{ContingencyType, LadderSpec, LadderTarget, OrderStatus};

type Core = CoreThread<RecordingClient>;

fn core_on(venue: &str, symbol: &str) -> Core {
    core_of(engine_on(venue, symbol, RecordingClient::default()), vec![], CoreConfig::default())
}

/// A long ladder of 3 on `venue`'s `symbol`: a limit entry at 100, the stop at 90, and three targets
/// of 1 at 110, 120 and 130. Its order client ids come back `[entry, stop, t1, t2, t3]`.
fn ladder(venue: &str, symbol: &str) -> OrderIntent {
    let spec = vike_model::build_ladder(&LadderSpec {
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 3.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        targets: [110.0, 120.0, 130.0].map(|price| LadderTarget { price, qty: 1.0 }).to_vec(),
        account: None,
    })
    .expect("a valid ladder");
    OrderIntent::OrderList(Box::new(spec))
}

fn feed(c: &mut Core, events: Vec<Event>) {
    for event in events {
        c.dispatch(Ingest::Event(event));
    }
}

/// Place the ladder and fill its entry: the stop and the targets are released, the stop ACCEPTED (an
/// amend changes a resting order only).
fn filled_ladder(c: &mut Core, venue: &str, symbol: &str) -> Vec<String> {
    let order_client_ids = c.apply_intent(ladder(venue, symbol), 0);
    feed(c, fill_events(&order_client_ids[0], venue, symbol, 1, 3.0, 100.0));
    accept(c, &order_client_ids[1]);
    order_client_ids
}

/// A PARTIAL fill of `qty` at `px` of the accepted order `order_client_id`.
fn partial_fill(c: &mut Core, order_client_id: &str, venue: &str, symbol: &str, qty: f64, px: f64) {
    sell_fill(c, order_client_id, (venue, symbol), qty, px, "t-", false);
}

/// One more SELL fill of `qty` at `px` of the accepted order `order_client_id` on `(venue, symbol)` under its own
/// trade id (`trade` prefixes it): partial, or the order's last (`full`).
fn sell_fill(
    c: &mut Core,
    order_client_id: &str,
    (venue, symbol): (&str, &str),
    qty: f64,
    px: f64,
    trade: &'static str,
    full: bool,
) {
    let events = fill_events(order_client_id, venue, symbol, -1, qty, px);
    let Some(Event::OrderFilled(mut f)) = events.last().cloned() else { unreachable!() };
    f.fill.trade_id = vike_model::events::TradeId::prefixed(trade, order_client_id);
    let wrap = if full {
        Event::OrderFilled(f.clone())
    } else {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id: f.order_client_id.clone(),
            fill: f.fill.clone(),
            timestamp: 0,
        })
    };
    feed(c, vec![Event::Fill(f.fill), wrap]);
}

fn canceled(c: &mut Core, order_client_id: &str) {
    c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
        order_client_id: order_client_id.to_string(),
        reason: "venue".to_string().into(),
        timestamp: 0,
    })));
}

/// The client's ids on every leg of [`ladder`]'s list (`ids` may name fewer legs than it has).
fn named_ladder(venue: &str, symbol: &str, ids: &[&str]) -> OrderIntent {
    let OrderIntent::OrderList(mut spec) = ladder(venue, symbol) else { unreachable!() };
    for (leg, id) in spec.legs.iter_mut().zip(ids) {
        leg.order_client_id = Some(id.to_string());
    }
    OrderIntent::OrderList(spec)
}

fn said(c: &Core, needle: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

/// A list that passes is placed as a bracket is: its entry alone is sent, every exit is HELD until the
/// entry fills, and each exit is an OUO leg of the book.
#[test]
fn a_ladder_sends_its_entry_alone_and_holds_every_exit() {
    let mut c = core_on("deribit", "X");
    let order_client_ids = c.apply_intent(ladder("deribit", "X"), 0);
    assert_eq!(order_client_ids.len(), 5, "entry, stop, three targets");
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "only the entry: {sent:?}");
    assert_eq!(sent[0].order_client_id, order_client_ids[0]);
    assert!(sent[0].linked_order_ids.is_empty(), "sent as a plain order");
    for exit in &order_client_ids[1..] {
        assert!(c.held_orders.contains_key(exit), "{exit} held pending the entry's fill");
        assert_eq!(c.contingency.kind_of(exit), Some(ContingencyType::Ouo));
    }
    assert_eq!(said(&c, "order list leg").len(), 4, "{:?}", c.recent);
}

/// Ruling 2: a target's fill shrinks the stop by what it filled, IN PLACE where the venue amends
/// (deribit), cancelling nothing; a partial fill of the stop takes what IT filled off the targets in
/// total, the farthest from its fill first, never every target by it.
#[test]
fn a_target_fill_shrinks_the_stop_and_a_stop_fill_spends_the_farthest_target_first() {
    let mut c = core_on("deribit", "X");
    let l = filled_ladder(&mut c, "deribit", "X");
    let (stop, t1, t2, t3) = (&l[1], &l[2], &l[3], &l[4]);
    assert_eq!(c.engine.client.submissions.len(), 5, "the entry's fill released all four exits");

    feed(&mut c, fill_events(t1, "deribit", "X", -1, 1.0, 110.0));
    assert_eq!(c.engine.client.modifies, vec![(stop.clone(), Some(2.0), None)]);
    assert!(
        c.engine.client.cancels.is_empty(),
        "OUO resizes; it cancels no sibling with size left"
    );
    c.dispatch(Ingest::Event(Event::OrderModified(OrderModified {
        order_client_id: stop.clone(),
        venue_order_id: None,
        new_qty: Some(2.0),
        new_price: None,
        new_trigger_price: None,
        timestamp: 0,
    })));
    accept(&mut c, t2);
    accept(&mut c, t3);

    partial_fill(&mut c, stop, "deribit", "X", 1.5, 90.0);
    assert_eq!(c.engine.client.cancels, vec![t3.clone()], "t3, the farthest, is spent whole");
    assert_eq!(c.engine.client.modifies.len(), 2, "{:?}", c.engine.client.modifies);
    assert_eq!(c.engine.client.modifies[1], (t2.clone(), Some(0.5), None), "t2 gives the rest");
    assert!(c.contingency.contains(stop), "a partial fill leaves the stop in the book");
}

/// Decision 0244 on a venue that cannot move a resting stop (binance's Algo Service): the target's
/// fill places the smaller stop FIRST and cancels nothing yet, so the position is never without one.
#[test]
fn a_binance_ladder_resizes_its_stop_new_first() {
    let mut c = core_on("binance", "BTCUSDT.P");
    let l = filled_ladder(&mut c, "binance", "BTCUSDT.P");
    feed(&mut c, fill_events(&l[2], "binance", "BTCUSDT.P", -1, 1.0, 110.0));
    let placed = c.engine.client.submissions.last().expect("the replacement");
    assert_eq!(
        (&placed.order_type, placed.qty, placed.trigger_price, placed.reduce_only),
        (&vike_model::OrderType::Stop, 2.0, Some(90.0), true)
    );
    assert_ne!(placed.order_client_id, l[1], "a new order");
    assert!(
        c.engine.client.cancels.is_empty(),
        "the old stop stands until the new one is answered"
    );
    assert_eq!(c.amends.len(), 1);
}

/// A ladder is refused WHOLE where an exit cannot be held as one (ig: its reduce-only stop) and where
/// an exit must cover a whole trade or position (fxcm, dukascopy, ctrader: `OUO_WHOLE_EXIT`): every
/// leg a terminal reject in words, nothing sent, nothing held.
#[test]
fn a_ladder_is_refused_whole_on_ig_fxcm_ctrader_and_dukascopy() {
    for (venue, words) in [
        ("ig", "REDUCE_ONLY_UNSUPPORTED"),
        ("fxcm", "OUO_WHOLE_EXIT"),
        ("ctrader", "OUO_WHOLE_EXIT"),
        ("dukascopy", "OUO_WHOLE_EXIT"),
    ] {
        let mut c = core_on(venue, "X");
        let order_client_ids = c.apply_intent(ladder(venue, "X"), 0);
        assert_eq!(order_client_ids.len(), 5, "{venue}: every leg is named");
        assert!(c.engine.client.submissions.is_empty(), "{venue}: nothing was sent");
        for leg in &order_client_ids {
            let status = c.engine.registry.get(leg).map(|mo| mo.status);
            assert_eq!(status, Some(OrderStatus::Rejected), "{venue}: {leg}");
        }
        assert!(c.held_orders.is_empty() && c.contingency.is_empty(), "{venue}: nothing held");
        assert_eq!(said(&c, "REFUSED WHOLE").len(), 1, "{venue}: {:?}", c.recent);
        assert!(!said(&c, words).is_empty(), "{venue}: {:?}", c.recent);
    }
}

/// The venue may fill both legs of an OCO before the cancel lands. The late fill is said on the ring
/// (judged at the drain-loop boundary, never on the fold), and so is a sibling that had already filled
/// some when its twin's fill sent the cancel; a cancel not yet answered says nothing.
#[test]
fn a_double_fill_is_said_on_the_ring() {
    let bracket = || {
        OrderIntent::Bracket(Box::new(vike_model::OrderBracket {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
            order_client_id: None,
        }))
    };
    let mut c = test_core();
    let l = c.apply_intent(bracket(), 0);
    feed(&mut c, fill_events(&l[0], "sim", "BTCUSDT", 1, 2.0, 100.0));
    feed(&mut c, fill_events(&l[1], "sim", "BTCUSDT", -1, 2.0, 95.0));
    assert!(c.engine.client.cancels.contains(&l[2]), "PREMISE: the take-profit's cancel is sent");
    c.note_contingency_races();
    assert!(said(&c, "DOUBLE FILL").is_empty(), "an unanswered cancel says nothing");
    feed(&mut c, fill_events(&l[2], "sim", "BTCUSDT", -1, 2.0, 110.0));
    c.note_contingency_races();
    assert_eq!(said(&c, "DOUBLE FILL").len(), 1, "{:?}", c.recent);
    assert!(c.contingency_races.is_empty(), "said once, then forgotten");

    let mut c = test_core();
    let l = c.apply_intent(bracket(), 0);
    feed(&mut c, fill_events(&l[0], "sim", "BTCUSDT", 1, 2.0, 100.0));
    accept(&mut c, &l[2]);
    partial_fill(&mut c, &l[2], "sim", "BTCUSDT", 0.5, 110.0);
    assert!(
        said(&c, "DOUBLE FILL").is_empty(),
        "PREMISE: an OCO leg's partial fill is no resolution"
    );
    feed(&mut c, fill_events(&l[1], "sim", "BTCUSDT", -1, 2.0, 95.0));
    assert_eq!(said(&c, "DOUBLE FILL").len(), 1, "both legs traded: {:?}", c.recent);
}

/// Across a restart: a ladder whose first target filled comes back with its stop at the RESIZED size
/// and still OUO (`Snapshot.contingencies` carries the type), so the next target's fill shrinks the stop
/// again where a book read as OCO would cancel it.
#[test]
fn a_half_filled_ladder_is_restored_with_its_resized_stop() {
    let dir = crate::scratch::Scratch::reserved("order-list-ladder-restart");
    let journal = Some(crate::runtime::JournalConfig::at(dir.to_path_buf()));
    let mut c = core_of(
        engine_on("deribit", "X", RecordingClient::default()),
        vec![],
        CoreConfig { journal, ..Default::default() },
    );
    let l = filled_ladder(&mut c, "deribit", "X");
    let (stop, t1, t2) = (&l[1], &l[2], &l[3]);
    feed(&mut c, fill_events(t1, "deribit", "X", -1, 1.0, 110.0));
    c.dispatch(Ingest::Event(Event::OrderModified(OrderModified {
        order_client_id: stop.clone(),
        venue_order_id: None,
        new_qty: Some(2.0),
        new_price: None,
        new_trigger_price: None,
        timestamp: 0,
    })));
    assert_eq!(c.engine.registry[stop].request.qty, 2.0, "PREMISE: the venue resized the stop");
    c.write_snap(1);
    c.journal.as_mut().expect("journal").flush().expect("flush");

    let state = crate::replay::restore_from_journal(&dir).expect("readable").expect("a Snapshot");
    let engine = Engine::from_snapshot(&state.engines[0], RecordingClient::default());
    let config = CoreConfig { contingencies: state.contingencies, ..Default::default() };
    let mut b = core_of(engine, vec![], config);
    assert_eq!(b.engine.registry[stop].request.qty, 2.0, "the stop comes back resized");
    assert_eq!(b.contingency.kind_of(stop), Some(ContingencyType::Ouo), "and OUO, not OCO");
    assert!(!b.contingency.contains(t1), "the filled target is gone from the book");

    feed(&mut b, fill_events(t2, "deribit", "X", -1, 1.0, 120.0));
    assert_eq!(b.engine.client.modifies, vec![(stop.clone(), Some(1.0), None)]);
    assert!(b.engine.client.cancels.is_empty(), "the stop is shrunk, never cancelled");
}

/// Ruling 1's format step through the core: a journal that only ever holds brackets keeps today's
/// format; the first order list's frame raises it to `vike_journal`'s order-list format (22), and so
/// does an accepted lone OUO leg.
#[test]
fn only_an_order_list_or_an_ouo_leg_raises_the_journal_format() {
    let journaled = |tag: &str| {
        let dir = crate::scratch::Scratch::reserved(tag);
        let journal = Some(crate::runtime::JournalConfig::at(dir.to_path_buf()));
        (core_with(CoreConfig { journal, ..Default::default() }), dir)
    };
    let version = |c: &mut Core, dir: &std::path::Path| {
        c.journal.as_mut().expect("journal").flush().expect("flush");
        vike_journal::Journal::latest_segment_version(dir).expect("readable")
    };
    let (mut c, dir) = journaled("order-list-format");
    let order = |i| Ingest::Command(vike_execution::Command::Order(i));
    c.dispatch(order(OrderIntent::Bracket(Box::new(vike_model::OrderBracket {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    }))));
    c.write_snap(1);
    assert_eq!(version(&mut c, dir.path()), Some(30), "a bracket's book keeps today's format");
    c.dispatch(order(ladder("sim", "BTCUSDT")));
    assert_eq!(
        version(&mut c, dir.path()),
        Some(30),
        "format 30 is the floor: an order list needs no more"
    );

    let (mut c, dir) = journaled("order-list-format-ouo-leg");
    let mut leg = market_request("ouo");
    leg.contingency_type = Some(ContingencyType::Ouo);
    leg.linked_order_ids = vec!["other".into()];
    c.apply_intent(OrderIntent::Submit(leg), 0);
    assert_eq!(
        version(&mut c, dir.path()),
        Some(30),
        "format 30 is the floor: an OUO leg needs no more"
    );
}

/// Fast target fills never leave the stop oversized. On binance the stop is replaced NEW first; a second
/// target's fill while that amend is in flight only moves the stop's wanted size (nothing is sent, so
/// nothing is refused), and the amend's end sends the one more resize: the stop ends at the position.
#[test]
fn two_target_fills_inside_one_amend_end_with_the_stop_at_the_position() {
    let (v, s) = ("binance", "BTCUSDT.P");
    let mut c = core_on(v, s);
    let l = filled_ladder(&mut c, v, s);
    let stop = l[1].clone();
    feed(&mut c, fill_events(&l[2], v, s, -1, 1.0, 110.0));
    let new1 = c.engine.client.submissions.last().expect("placed").order_client_id.clone();
    assert_ne!(new1, stop, "the first resize is a new stop, placed first");
    feed(&mut c, fill_events(&l[3], v, s, -1, 1.0, 120.0));
    assert_eq!(
        c.engine.client.submissions.last().unwrap().order_client_id,
        new1,
        "nothing new yet"
    );

    accept(&mut c, &new1);
    c.drive_amends(20);
    canceled(&mut c, &stop);
    c.drive_amends(21);
    let placed = c.engine.client.submissions.last().expect("the one more resize");
    assert_eq!((&placed.order_type, placed.qty), (&vike_model::OrderType::Stop, 1.0));
    let new2 = placed.order_client_id.clone();
    accept(&mut c, &new2);
    c.drive_amends(22);
    canceled(&mut c, &new1);
    c.drive_amends(23);

    assert!(c.amends.is_empty(), "every amend ended");
    assert_eq!(c.engine.registry[&new2].request.qty, 1.0, "the stop is the position: 3 - 1 - 1");
    assert!(c.contingency.contains(&new2), "and it is the ladder's stop now");
    assert!(said(&c, "refused").is_empty(), "no amend was refused: {:?}", c.recent);
}

/// No fill is spent twice after a re-key. The old stop's fill before the re-key is driven by its own
/// event; its fill AFTER the re-key reaches no drive, and the amend's end drives only that one: the
/// targets give 0.25 + 0.25 for the stop's 0.5, and the stop is resized to what is left (1.5).
#[test]
fn a_stop_fill_on_both_sides_of_a_re_key_is_spent_once() {
    let (v, s) = ("binance", "BTCUSDT.P");
    let mut c = core_on(v, s);
    let l = filled_ladder(&mut c, v, s);
    let (stop, t3) = (l[1].clone(), l[4].clone());
    accept(&mut c, &l[3]);
    accept(&mut c, &t3);
    feed(&mut c, fill_events(&l[2], v, s, -1, 1.0, 110.0));
    let new1 = c.engine.client.submissions.last().expect("placed").order_client_id.clone();
    sell_fill(&mut c, &stop, (v, s), 0.25, 90.0, "a-", false);
    accept(&mut c, &new1);
    c.drive_amends(20); // answered: the book keys the stop by `new1` now
    sell_fill(&mut c, &stop, (v, s), 0.25, 90.0, "b-", false);
    canceled(&mut c, &stop);
    c.drive_amends(21);

    let t3_sizes: Vec<Option<f64>> =
        c.engine.client.modifies.iter().filter(|m| m.0 == t3).map(|m| m.1).collect();
    assert_eq!(
        t3_sizes,
        vec![Some(0.75), Some(0.5)],
        "0.25 before the re-key, 0.25 after, once each"
    );
    let placed = c.engine.client.submissions.last().expect("the one more resize");
    assert_eq!(placed.qty, 1.5, "the stop is resized to the position: 3 - 1 - 0.5");
}

/// ...and an amend that ends because the OLD order filled drives only what of that fill was not driven:
/// each target is cancelled once, and none is resized again.
#[test]
fn an_amend_ended_by_the_old_stop_filling_spends_the_ladder_once() {
    let (v, s) = ("binance", "BTCUSDT.P");
    let mut c = core_on(v, s);
    let l = filled_ladder(&mut c, v, s);
    let (stop, t2, t3) = (l[1].clone(), l[3].clone(), l[4].clone());
    accept(&mut c, &t2);
    accept(&mut c, &t3);
    feed(&mut c, fill_events(&l[2], v, s, -1, 1.0, 110.0));
    let new1 = c.engine.client.submissions.last().expect("placed").order_client_id.clone();
    sell_fill(&mut c, &stop, (v, s), 0.25, 90.0, "a-", false);
    accept(&mut c, &new1);
    c.drive_amends(20);
    sell_fill(&mut c, &stop, (v, s), 2.75, 90.0, "b-", true);
    c.drive_amends(21);

    for leg in [&t2, &t3] {
        let times = c.engine.client.cancels.iter().filter(|x| *x == leg).count();
        assert_eq!(times, 1, "{leg} cancelled once: {:?}", c.engine.client.cancels);
    }
    assert_eq!(c.engine.client.modifies.iter().filter(|m| m.0 == t3).count(), 1, "resized once");
    assert!(said(&c, "NOT cancelled").is_empty(), "{:?}", c.recent);
    assert!(c.contingency.is_empty(), "the ladder resolved");
}

/// A dead ladder TARGET never cancels the stop, whatever `oco_cancel_sibling_on_dead_exit` says (OUO
/// resizes; it does not cancel), while the knob keeps its meaning for a bracket: there the dead stop's
/// take-profit IS cancelled.
#[test]
fn a_dead_ladder_target_keeps_its_stop_with_the_knob_on_and_a_bracket_keeps_the_knob() {
    let knob = || CoreConfig { oco_cancel_sibling_on_dead_exit: true, ..Default::default() };
    let expire = |c: &mut Core, order_client_id: &str| {
        c.dispatch(Ingest::Event(Event::OrderExpired(vike_model::events::OrderExpired {
            order_client_id: order_client_id.to_string(),
            timestamp: 0,
        })));
    };
    let mut c = core_of(engine_on("deribit", "X", RecordingClient::default()), vec![], knob());
    let l = filled_ladder(&mut c, "deribit", "X");
    accept(&mut c, &l[2]);
    expire(&mut c, &l[2]);
    assert!(c.engine.client.cancels.is_empty(), "the stop stands: {:?}", c.engine.client.cancels);
    assert!(c.contingency.contains(&l[1]) && !c.contingency.contains(&l[2]));

    let mut c = core_of(engine_on("deribit", "X", RecordingClient::default()), vec![], knob());
    let b = c.apply_intent(
        OrderIntent::Bracket(Box::new(vike_model::OrderBracket {
            venue: "deribit".into(),
            symbol: "X".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
            order_client_id: None,
        })),
        0,
    );
    feed(&mut c, fill_events(&b[0], "deribit", "X", 1, 2.0, 100.0));
    accept(&mut c, &b[1]);
    expire(&mut c, &b[1]);
    assert_eq!(c.engine.client.cancels, vec![b[2].clone()], "the knob cancels the take-profit");
}

/// A spot ladder (a Cash symbol): the stop (the position) and the targets (parts of it) are ONE claim on
/// the holding, so every target is released; a plain sell past the holding is still refused.
#[test]
fn a_spot_ladder_releases_its_targets_and_a_plain_extra_sell_is_still_refused() {
    let mut c = core_on("binance", "BTCUSDT");
    c.engine.account.admit_margin_mode("BTCUSDT", Some(vike_model::MarginMode::Cash));
    let l = filled_ladder(&mut c, "binance", "BTCUSDT");
    let sent: Vec<String> =
        c.engine.client.submissions.iter().map(|r| r.order_client_id.clone()).collect();
    assert_eq!(sent, l, "the entry, the stop and every target reached the venue: {:?}", c.recent);
    assert!(c.engine.linked_exits.is_none(), "the group is cleared after each release");
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        })),
        5,
    );
    assert_eq!(c.engine.client.submissions.len(), 5, "the plain sell never reached the venue");
}

/// A client's retry-safe list names every leg's id: the same list sent again places nothing new and is
/// refused whole in a reused id's words; a list naming only some ids is refused at the model's door,
/// with nothing minted or sent.
#[test]
fn a_retried_order_list_with_its_own_ids_places_nothing_new() {
    let ids = ["e1", "s1", "a1", "b1", "c1"];
    let mut c = core_on("deribit", "X");
    let order_client_ids = c.apply_intent(named_ladder("deribit", "X", &ids), 0);
    assert_eq!(order_client_ids, ids.map(String::from).to_vec(), "the legs carry the client's ids");
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert!(c.apply_intent(named_ladder("deribit", "X", &ids), 1).is_empty());
    assert_eq!(c.engine.client.submissions.len(), 1, "the retry sent nothing");
    assert_eq!(c.held_orders.len(), 4, "and held nothing more");
    assert!(!said(&c, "order client id e1 is already used").is_empty(), "{:?}", c.recent);
    assert_eq!(said(&c, "REFUSED WHOLE").len(), 1);

    let mut c = core_on("deribit", "X");
    assert!(c.apply_intent(named_ladder("deribit", "X", &ids[..2]), 0).is_empty());
    assert!(c.engine.client.submissions.is_empty() && c.engine.registry.is_empty());
    assert!(!said(&c, "every leg's order id or none").is_empty(), "{:?}", c.recent);
}
