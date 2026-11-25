//! `desktop_bracket_tickets_on_the_live_node`: what it proves and how to run it are in the parent's
//! module doc.

use vike_execution::OrderView;
use vike_model::{OrderStatus, OrderType};

use super::{
    Desk, FAR, FX_TICK, ORDER_WAIT, SIZE_FX, SIZE_IG, assert_flat, boot, engine_symbols,
    one_per_process, staged,
};

#[test]
#[ignore = "LIVE: sends TP/SL brackets (far limits) on the ig, dukascopy and cTrader DEMO accounts; by hand only"]
fn desktop_bracket_tickets_on_the_live_node() {
    one_per_process();
    let Some(boot) = boot() else { return };
    let Some((ig, dukascopy, ctrader)) = engine_symbols(boot) else { return };
    let rows = [
        staged("ig", &ig, 1.0, FX_TICK),
        staged("dukascopy", &dukascopy, 1000.0, FX_TICK),
        staged("ctrader", &ctrader, 1000.0, FX_TICK),
    ];
    let Some(mut d) = Desk::launch(boot) else { return };
    d.stage(&rows);
    for venue in ["ig", "dukascopy", "ctrader"] {
        assert_flat(&d.snapshot(), venue, "REFUSED before any order");
    }
    d.await_staged(&rows);
    let mut report: Vec<(&str, bool)> = Vec::new();

    // ig and dukascopy: refused WHOLE, nothing sent.
    for (venue, symbol, size, kind) in
        [("ig", &ig, SIZE_IG, "stop"), ("dukascopy", &dukascopy, SIZE_FX, "limit")]
    {
        let w = d.open_trade(venue, symbol);
        let before = d.order_client_ids();
        let price = d.reference(venue, symbol) * FAR;
        d.opened.push((venue.to_string(), symbol.to_string()));
        if let Err(why) = d.buy(w, Some(price), size, Some("1")) {
            eprintln!("RESULT {venue} {symbol}: the desktop sent nothing: {why}");
            report.push((venue, false));
            continue;
        }
        let legs = d.await_orders(&before, venue, 3, |o| o.iter().all(|o| o.status.is_terminal()));
        d.placed.extend(legs.iter().map(|o| o.order_client_id.clone()));
        let said =
            |o: &OrderView, words: &str| o.reason.as_deref().is_some_and(|r| r.contains(words));
        let culprit = format!("REDUCE_ONLY_UNSUPPORTED: kind={kind} venue={venue}");
        let whole = format!("bracket for {venue} REFUSED WHOLE");
        let note = d.snapshot().recent_events.iter().any(|e| e.contains(&whole));
        for o in &legs {
            eprintln!(
                "  leg {} {:?} side {} {:?}: {}",
                o.order_client_id,
                o.order_type,
                o.side,
                o.status,
                o.reason.as_deref().unwrap_or("")
            );
        }
        eprintln!(
            "RESULT {venue} {symbol}: {} legs; `{whole}` in the recent events: {note}; strip `{}`",
            legs.len(),
            d.strip(w)
        );
        let ok = legs.len() == 3
            && legs.iter().all(|o| o.status == OrderStatus::Rejected)
            && legs.iter().filter(|o| said(o, &culprit)).count() == 1
            && legs.iter().filter(|o| said(o, "BRACKET_ATOMIC_REFUSED")).count() == 2;
        report.push((venue, ok));
        d.cleanup();
        assert_flat(&d.snapshot(), venue, "AFTER the bracket");
    }

    // cTrader: the SAME ticket is accepted whole. The entry is a far limit (the thin client has no
    // price to send a market order from), so it rests, its two exits are held for it, and
    // cancelling the entry takes them with it.
    let w = d.open_trade("ctrader", &ctrader);
    let before = d.order_client_ids();
    let price = d.reference("ctrader", &ctrader) * FAR;
    d.opened.push(("ctrader".to_string(), ctrader.clone()));
    let sent = d.buy(w, Some(price), SIZE_FX, Some("1"));
    let held = |d: &Desk| d.snapshot().held_exits.iter().filter(|h| h.venue == "ctrader").count();
    let rests = |d: &Desk| {
        let new = d.new_orders(&before, "ctrader");
        new.iter().any(|o| o.status == OrderStatus::Accepted) && held(d) == 2
    };
    if sent.is_ok() {
        d.wait("the entry to rest and its two exits to be held", ORDER_WAIT, rests);
    }
    let legs = d.new_orders(&before, "ctrader");
    d.placed.extend(legs.iter().map(|o| o.order_client_id.clone()));
    for o in &legs {
        eprintln!(
            "  leg {} {:?} side {} {:?}: {}",
            o.order_client_id,
            o.order_type,
            o.side,
            o.status,
            o.reason.as_deref().unwrap_or("")
        );
    }
    for h in d.snapshot().held_exits.iter().filter(|h| h.venue == "ctrader") {
        eprintln!(
            "  held exit {} {:?} side {} price {:?} trigger {:?} for {:?}",
            h.order_client_id, h.order_type, h.side, h.price, h.trigger_price, h.parent_order_id
        );
    }
    let accepted = sent.is_ok() && legs.len() == 1 && rests(&d);
    eprintln!(
        "RESULT ctrader {ctrader} bracket: sent {sent:?}; {} orders, {} held exits; strip `{}`",
        legs.len(),
        held(&d),
        d.strip(w)
    );
    let parent = legs.iter().find(|o| o.order_type == OrderType::Limit && o.side == 1);
    let cancelled = parent.is_some_and(|p| d.cancel(&p.order_client_id) == OrderStatus::Canceled);
    let gone = d.wait("the exits to go with their entry", ORDER_WAIT, |d| held(d) == 0);
    eprintln!("RESULT ctrader after the entry's cancel: held exits {}", held(&d));
    report.push(("ctrader", accepted && cancelled && gone));
    d.cleanup();
    for venue in ["ig", "dukascopy", "ctrader"] {
        assert_flat(&d.snapshot(), venue, "AFTER the brackets");
    }
    d.shut();
    eprintln!("RESULT brackets: {report:?}");
    assert!(report.iter().all(|(_, ok)| *ok), "a bracket did not end as expected: {report:?}");
}
