//! An operator's cancel that the order registry cannot act on is SAID, not accepted in silence
//! (`CoreThread::report_unheld_cancel`, called from the external `Command::Order` arm of `dispatch`).
//!
//! # The defect these gate
//!
//! Measured on the CI box v0.1.84 after a restart (Hyperliquid, IBKR, OKX, Bybit, Deribit, cTrader, OANDA): an order the operator
//! placed through the node is held as an `UnknownOrder` row and is NOT in the engine registry, so
//! `cancel <coid>` and `mass-cancel <venue>` were Acked, logged `command accepted`, sent nothing to
//! the venue and emitted no event; the order kept resting and the operator could not tell.
//!
//! White-box for the reason `recon_held.rs` states: the held-row store and the dispatch arm are private
//! to `super`. Each case drives `dispatch` (the same call a `Command` makes) on a synchronous core.

use super::*;
use tracing_test::traced_test;
use vike_exec::testing::RecordingClient;
use vike_model::{OrderRequest, OrderStatus, OrderStatusReport};

use crate::runtime::test_support::{core_of, engine_on};

const VENUE: &str = "bybit";
const SYMBOL: &str = "BTCUSDT";

fn core() -> CoreThread<RecordingClient> {
    core_of(engine_on(VENUE, SYMBOL, RecordingClient::default()), Vec::new(), CoreConfig::default())
}

/// A resting venue order that no registry of ours holds, as the venue's open-order read reports it.
/// `coid` is what the venue echoes back: ours (IBKR, Bybit) or its own spelling (Hyperliquid's hex).
fn venue_order(venue_id: &str, coid: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        venue_order_id: venue_id.into(),
        client_order_id: Some(coid.to_string()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: OrderStatus::Accepted,
        ts: 1_000,
    }
}

/// One pass under `quarantine` (what the CI box runs): every unknown order is HELD, none adopted.
fn pass(orders: Vec<OrderStatusReport>) -> ReconcileReports {
    ReconcileReports {
        venue: VENUE.into(),
        since: 0,
        orders,
        fills: Vec::new(),
        positions: Vec::new(),
        policy: vike_exec::recon::ReconPolicy {
            default: vike_exec::recon::ReconMode::Quarantine,
            ..Default::default()
        },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }
}

/// The ids of the held `UnknownOrder` rows after the pass (the ids `trade recon ls` prints).
fn hold(c: &mut CoreThread<RecordingClient>, orders: Vec<OrderStatusReport>) -> Vec<u64> {
    c.reconcile_reports(pass(orders));
    c.recon_alerts
        .iter()
        .filter(|(_, h)| h.kind == DivergenceKind::UnknownOrder)
        .map(|(&id, _)| id)
        .collect()
}

fn command(c: &mut CoreThread<RecordingClient>, intent: OrderIntent) {
    c.dispatch(Ingest::Command(Command::Order(intent)));
}

/// A live order in the engine registry, placed the way a CLI submit is.
fn rest(c: &mut CoreThread<RecordingClient>, coid: &str) {
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: coid.into(),
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            side: 1,
            qty: 1.0,
            price: Some(100.0),
            order_type: vike_model::OrderType::Limit,
            ..Default::default()
        })),
        0,
    );
}

/// The ring lines (what `recent` shows) that start with `prefix`.
fn lines(c: &CoreThread<RecordingClient>, prefix: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.starts_with(prefix)).map(|l| l.to_string()).collect()
}

/// THE MEASURED CASE: the venue reports the order under our coid, the registry does not hold it. The
/// cancel says so, names the held row, and still sends NOTHING to the venue.
#[test]
#[traced_test]
fn a_cancel_of_an_unregistered_coid_is_rejected_out_loud_naming_the_held_row() {
    let mut c = core();
    let rows = hold(&mut c, vec![venue_order("v-1", "b6ibkrlim")]);
    assert_eq!(rows.len(), 1, "premise: the order is held as ONE UnknownOrder row");

    command(&mut c, OrderIntent::Cancel("b6ibkrlim".into()));

    let said = lines(&c, "OrderCancelRejected b6ibkrlim (");
    assert_eq!(said.len(), 1, "one rejection line in the ring: {:?}", c.recent);
    assert!(said[0].contains(&format!("UnknownOrder #{}", rows[0])), "names the row: {}", said[0]);
    assert!(said[0].contains("not in this node's order registry"), "{}", said[0]);
    assert!(said[0].contains("cancel it at the venue"), "{}", said[0]);
    assert!(c.engine.client.cancels.is_empty(), "no venue call: this is part 1, honesty only");
    assert_eq!(c.engine.dropped_unknown_coid, 0, "the line is not a venue event the fold drops");
    assert!(logs_contain("cancel NOT sent"), "a WARN at the command");
}

/// A venue that spells the coid its own way (Hyperliquid's hex) leaves no row that names it: the
/// reason lists the rows the node holds instead of claiming a match it cannot prove.
#[test]
fn a_row_that_does_not_name_the_coid_is_listed_not_claimed() {
    let mut c = core();
    let rows = hold(&mut c, vec![venue_order("63531877324", "0xb395aaaa")]);

    command(&mut c, OrderIntent::Cancel("b6hllim".into()));

    let said = lines(&c, "OrderCancelRejected b6hllim (");
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("holds 1 UnknownOrder row(s)"), "{}", said[0]);
    assert!(said[0].contains(&format!("#{}", rows[0])), "{}", said[0]);
    assert!(c.engine.client.cancels.is_empty());
}

/// Nothing held anywhere: still said (a mistyped id must not look accepted), without inventing a row.
#[test]
fn a_cancel_of_an_id_nothing_knows_is_rejected_without_a_row() {
    let mut c = core();

    command(&mut c, OrderIntent::Cancel("typo".into()));

    let said = lines(&c, "OrderCancelRejected typo (");
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("mistyped id"), "{}", said[0]);
    assert!(!said[0].contains("UnknownOrder"), "{}", said[0]);
}

/// PRESERVE: a registry order is cancelled exactly as before and the rejection never appears.
#[test]
fn a_cancel_of_a_registered_order_is_unchanged() {
    let mut c = core();
    rest(&mut c, "r1");

    command(&mut c, OrderIntent::Cancel("r1".into()));

    assert_eq!(c.engine.client.cancels, vec!["r1".to_string()], "the cancel reaches the client");
    assert!(lines(&c, "OrderCancelRejected").is_empty(), "{:?}", c.recent);
}

/// The boundary: only the OPERATOR path speaks. A strategy's cancel of a registry-less order (a
/// maker's `cancel_tagged` of a restored quote before the first pass) goes through `apply_intent`
/// and stays as quiet as the restore arithmetic in `crates/vike-core/CLAUDE.md` assumes.
#[test]
fn a_cancel_lowered_outside_the_command_path_stays_silent() {
    let mut c = core();
    hold(&mut c, vec![venue_order("v-1", "b6lim")]);

    c.apply_intent(OrderIntent::Cancel("b6lim".into()), 0);

    assert!(lines(&c, "OrderCancelRejected").is_empty(), "{:?}", c.recent);
    assert!(c.engine.client.cancels.is_empty());
}

/// MIX: a mass-cancel cancels what the registry holds, as before, and says once that the held
/// `UnknownOrder` rows of its scope were not reached.
#[test]
#[traced_test]
fn a_mass_cancel_with_a_mix_cancels_the_registry_and_names_what_it_cannot_reach() {
    let mut c = core();
    rest(&mut c, "r1");
    let rows = hold(&mut c, vec![venue_order("v-1", "b6lim"), venue_order("v-2", "b6lim2")]);
    assert_eq!(rows.len(), 2, "premise");

    command(
        &mut c,
        OrderIntent::MassCancel { venue: Some(VENUE.into()), symbol: None, account: None },
    );

    assert_eq!(c.engine.client.cancels, vec!["r1".to_string()], "the registry order is cancelled");
    let said = lines(&c, "MassCancel on bybit: 2 order(s)");
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    for id in &rows {
        assert!(said[0].contains(&format!("#{id}")), "names row #{id}: {}", said[0]);
    }
    assert!(said[0].contains("cancel them at the venue"), "{}", said[0]);
    assert!(logs_contain("NOT cancelled"), "a WARN at the command");
}

/// A mass-cancel with no held row in its scope leaves the ring as it was: no noise on the common case.
#[test]
fn a_mass_cancel_with_nothing_unreached_adds_no_line() {
    let mut c = core();
    rest(&mut c, "r1");
    let before = c.recent.len();

    command(
        &mut c,
        OrderIntent::MassCancel { venue: Some(VENUE.into()), symbol: None, account: None },
    );

    assert_eq!(c.engine.client.cancels, vec!["r1".to_string()]);
    assert!(lines(&c, "MassCancel on").is_empty(), "{:?}", c.recent);
    assert_eq!(c.recent.len(), before, "no ring line at all");
}

// ---- review fixes: the panic button, the exact row, the restored entry, the symbol, the attribute ----

/// The panic button's cancel leg walks the same registry, so it skips the same held rows: it cancels
/// what the registry holds, as before, and says once which rows it could not reach.
#[test]
fn a_market_exit_says_which_held_rows_its_cancel_leg_could_not_reach() {
    let mut c = core();
    rest(&mut c, "r1");
    let rows = hold(&mut c, vec![venue_order("v-1", "b6lim"), venue_order("v-2", "b6lim2")]);
    assert_eq!(rows.len(), 2, "premise");

    command(&mut c, OrderIntent::MarketExit { venue: Some(VENUE.into()), account: None });

    assert_eq!(c.engine.client.cancels, vec!["r1".to_string()], "the registry order is cancelled");
    let said = lines(&c, "MarketExit (its cancel leg) on bybit: 2 order(s)");
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    for id in &rows {
        assert!(said[0].contains(&format!("#{id}")), "names row #{id}: {}", said[0]);
    }
}

/// An account named with NO venue is refused by the exit itself; the helper adds nothing on top.
#[test]
fn a_market_exit_naming_an_account_without_a_venue_adds_no_line() {
    let mut c = core();
    hold(&mut c, vec![venue_order("v-1", "b6lim")]);
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");

    command(&mut c, OrderIntent::MarketExit { venue: None, account: Some(alt) });

    assert!(lines(&c, "MarketExit (its cancel leg)").is_empty(), "{:?}", c.recent);
}

/// `b6lim` is a substring of the held `b6lim2`'s line; so are `Some`, `BTCUSDT` and the venue's name.
/// None of them is that order, and none may be reported as "held as UnknownOrder #N".
#[test]
fn a_cancel_names_a_row_only_on_the_exact_coid() {
    let mut c = core();
    let rows = hold(&mut c, vec![venue_order("v-2", "b6lim2")]);
    assert_eq!(rows.len(), 1, "premise");

    for wrong in ["b6lim", "Some", SYMBOL, VENUE] {
        command(&mut c, OrderIntent::Cancel(wrong.into()));
        let said = lines(&c, &format!("OrderCancelRejected {wrong} ("));
        assert_eq!(said.len(), 1, "{wrong}: {:?}", c.recent);
        assert!(!said[0].contains("holds it as UnknownOrder"), "{wrong} named a row: {}", said[0]);
        assert!(said[0].contains("holds 1 UnknownOrder row(s)"), "{wrong}: {}", said[0]);
    }
    command(&mut c, OrderIntent::Cancel("b6lim2".into()));
    let said = lines(&c, "OrderCancelRejected b6lim2 (");
    assert!(said[0].contains(&format!("holds it as UnknownOrder #{}", rows[0])), "{}", said[0]);
}

/// An order the ownership file remembers and the first pass has not judged is not "never placed
/// here, or a mistyped id".
#[test]
fn a_coid_the_ownership_file_remembers_is_not_called_a_typo() {
    let mut c = core();
    c.restored_orders.insert(
        "r9".into(),
        crate::order_owners::OwnedOrder { coid: "r9".into(), ..Default::default() },
    );

    command(&mut c, OrderIntent::Cancel("r9".into()));

    let said = lines(&c, "OrderCancelRejected r9 (");
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("first reconcile pass has not judged it"), "{}", said[0]);
    assert!(!said[0].contains("mistyped"), "{}", said[0]);
    assert!(c.engine.client.cancels.is_empty(), "still nothing sent");
}

/// With a symbol the held rows cannot be filtered (they record no symbol), so the count is an upper
/// bound and the line says so; the symbol itself is not echoed.
#[test]
fn a_mass_cancel_with_a_symbol_says_up_to_n() {
    let mut c = core();
    hold(&mut c, vec![venue_order("v-1", "b6lim"), venue_order("v-2", "b6lim2")]);

    command(
        &mut c,
        OrderIntent::MassCancel {
            venue: Some(VENUE.into()),
            symbol: Some("ETHUSDT".into()),
            account: None,
        },
    );

    let said = lines(&c, "MassCancel on bybit (a symbol was named): up to 2 order(s)");
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("not filed by symbol"), "{}", said[0]);
    assert!(!said[0].contains("ETHUSDT"), "the wire text is not echoed: {}", said[0]);
}

/// A symbol with no venue is ignored by the cancel itself (it cancels nothing), so the line is not
/// written either.
#[test]
fn a_mass_cancel_with_a_symbol_and_no_venue_adds_no_line() {
    let mut c = core();
    hold(&mut c, vec![venue_order("v-1", "b6lim")]);

    command(
        &mut c,
        OrderIntent::MassCancel { venue: None, symbol: Some(SYMBOL.into()), account: None },
    );

    assert!(lines(&c, "MassCancel on").is_empty(), "{:?}", c.recent);
}

/// The helpers `dispatch` reaches only on a miss stay out of line (`#[inline(never)]`, as
/// `crates/vike-core/CLAUDE.md` asks of every cold helper `dispatch` calls); inlined, their
/// `format!`s and iterator chains would grow the function the core-hop gate measures. This pins the
/// attribute; what it buys is measured by the `latency` job, not here.
#[test]
fn the_cold_helpers_stay_out_of_line_of_dispatch() {
    let src = include_str!("../refusals.rs");
    let all: Vec<&str> = src.lines().collect();
    for name in ["report_unheld_cancel", "report_unreached_rows", "unheld_cancel_reason"] {
        let at = all
            .iter()
            .position(|l| l.contains(&format!("fn {name}(")))
            .unwrap_or_else(|| panic!("{name} not found in refusals.rs"));
        assert_eq!(all[at - 1].trim(), "#[inline(never)]", "{name} lost its attribute");
    }
}
