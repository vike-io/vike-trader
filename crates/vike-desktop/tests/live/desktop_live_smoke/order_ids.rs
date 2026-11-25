//! `desktop_order_ids_are_session_unique_on_the_live_node` and the helpers only it uses: what it
//! proves and how to run it are in the parent's module doc.

use std::collections::HashSet;

use eframe::egui;
use vike_app_core::ui::window::WindowKind;
use vike_execution::OrderView;
use vike_model::{OrderStatus, OrderType};

use super::{
    DATA_WAIT, Desk, FAR, ORDER_CLIENT_ID_AT, SIZE_FX, assert_flat, boot, one_per_process, staged,
};

/// `(prefix, session, sequence)` of a desktop id. A v2 id (decision 0270) is `G…`, a manual GUI order:
/// its unique part is the session's random base PLUS the sequence, so no session can be read off it
/// and it reads `("G", "", unique part)` ([`same_session`] judges by distance instead). A v1 id is
/// `^(ui|dom|polymarket|opt)[0-9a-f]{8}[0-9]+$`.
fn order_client_id_parts(order_client_id: &str) -> Option<(&str, &str, u64)> {
    use vike_model::orders::client_id::{
        OrderClientIdReading, OrderClientIdSource, parse_order_client_id,
    };
    if let Some(OrderClientIdReading::V2(p)) = parse_order_client_id(order_client_id) {
        let unique = u64::from_str_radix(p.unique, 36).ok();
        return unique.filter(|_| p.src == OrderClientIdSource::Gui).map(|u| ("G", "", u));
    }
    let prefix =
        ["poly", "dom", "opt", "ui"].into_iter().find(|p| order_client_id.starts_with(p))?;
    let (session, seq) = order_client_id[prefix.len()..].split_at_checked(8)?;
    let hex = session.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let digits = !seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit());
    if hex && digits { Some((prefix, session, seq.parse().ok()?)) } else { None }
}

/// Whether two [`order_client_id_parts`] are of one session: one session string (v1), or (v2) unique parts
/// within 2^40 of each other, the most one session mints, while two random bases land that close
/// about once in two million draws.
fn same_session(a: (&str, &str, u64), b: (&str, &str, u64)) -> bool {
    a.1 == b.1 && (!a.1.is_empty() || a.2.abs_diff(b.2) < 1 << 40)
}

/// The order client id test's far limit through the ticket of a window on [`ORDER_CLIENT_ID_AT`], read back.
fn place_far_limit(d: &mut Desk, id: egui::Id) -> OrderView {
    let (venue, symbol) = ORDER_CLIENT_ID_AT;
    let before = d.order_client_ids();
    let price = d.reference(venue, symbol) * FAR;
    d.opened.push((venue.to_string(), symbol.to_string()));
    d.buy(id, Some(price), SIZE_FX, None)
        .unwrap_or_else(|why| panic!("the ticket will not send: {why}"));
    let answered = |o: &[OrderView]| {
        o.iter().all(|o| !matches!(o.status, OrderStatus::Initialized | OrderStatus::Submitted))
    };
    let new = d.await_orders(&before, venue, 1, answered);
    let strip = d.strip(id);
    let order = new
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no order reached the node; strip: {strip}"));
    d.placed.push(order.order_client_id.clone());
    assert!(!strip.contains("already used"), "the node refused a repeated id: {strip}");
    eprintln!(
        "  placed {} {:?} side {} {} {} @ {:?} -> {:?} {}",
        order.order_client_id,
        order.order_type,
        order.side,
        order.symbol,
        order.qty,
        order.price,
        order.status,
        order.reason.as_deref().unwrap_or("")
    );
    order
}

/// Load the named layout, which holds `windows` windows, through the File menu, as a trader does.
/// It is read off the window list: the status line the load writes is overwritten by the link's
/// own line within a frame (measured), so it is no witness.
fn load_layout(d: &mut Desk, name: &'static str, windows: usize) {
    d.click(None, "the File menu", |l| l == "File");
    d.click(None, "Load layout", |l| l.starts_with("Load layout"));
    d.click(None, "the layout's row", move |l| l == name);
    let loaded = d.wait("the layout", DATA_WAIT, |d| {
        let wins = &d.app().windows;
        wins.len() == windows && wins.iter().all(|w| w.kind == WindowKind::Chart)
    });
    assert!(loaded, "layout '{name}' did not load: {}", d.dump());
}

#[test]
#[ignore = "LIVE: places and cancels far limit orders on the cTrader DEMO account of the node; by hand only"]
fn desktop_order_ids_are_session_unique_on_the_live_node() {
    one_per_process();
    let Some(boot) = boot() else { return };
    let (venue, symbol) = ORDER_CLIENT_ID_AT;
    let rows = [staged(venue, symbol, 1000.0, 0.0)];

    // Launch 1, on the three-window workspace.
    let Some(mut d) = Desk::launch(boot) else { return };
    d.stage(&rows);
    assert_flat(&d.snapshot(), venue, "REFUSED before any order");
    d.await_staged(&rows);
    let w = d.open_trade(venue, symbol);
    let first = place_far_limit(&mut d, w);
    let first_end = d.cancel(&first.order_client_id);
    d.shut();

    // Launch 2: a restart on the same workspace.
    let Some(mut d) = Desk::launch(boot) else { return };
    d.stage(&rows);
    d.await_staged(&rows);
    let w = d.open_trade(venue, symbol);
    let second = place_far_limit(&mut d, w);
    let second_end = d.cancel(&second.order_client_id);
    // ...then a layout with FEWER windows, in the same session. The load takes the Trade window
    // with it (a layout holds chart windows only), so the trader opens a new one.
    load_layout(&mut d, "fewer", 1);
    assert_eq!(d.app().windows.len(), 1, "the layout holds one chart window");
    let w = d.open_trade(venue, symbol);
    let third = place_far_limit(&mut d, w);
    let third_end = d.cancel(&third.order_client_id);
    assert_flat(&d.snapshot(), venue, "AFTER the three orders");
    d.shut();

    let rows = [(&first, first_end), (&second, second_end), (&third, third_end)];
    for (o, end) in &rows {
        eprintln!("RESULT {} -> {:?}, then {end:?}", o.order_client_id, o.status);
    }
    let parts: Vec<(&str, &str, u64)> = rows
        .iter()
        .map(|(o, _)| {
            order_client_id_parts(&o.order_client_id)
                .unwrap_or_else(|| panic!("bad id shape {}", o.order_client_id))
        })
        .collect();
    for (o, end) in &rows {
        assert_eq!(
            o.status,
            OrderStatus::Accepted,
            "{} was not accepted: {:?}",
            o.order_client_id,
            o.reason
        );
        assert_eq!(*end, OrderStatus::Canceled, "{} was not cancelled", o.order_client_id);
        let limit_buy = o.order_type == OrderType::Limit && o.side == 1;
        assert!(limit_buy, "{} is a limit buy", o.order_client_id);
    }
    assert!(!same_session(parts[0], parts[1]), "a restart draws a new session: {parts:?}");
    assert!(same_session(parts[1], parts[2]), "a layout load keeps the session: {parts:?}");
    assert!(parts[2].2 > parts[1].2, "the sequence grows across a layout load");
    let ids: HashSet<&str> = rows.iter().map(|(o, _)| o.order_client_id.as_str()).collect();
    assert_eq!(ids.len(), 3, "three distinct ids");
}
