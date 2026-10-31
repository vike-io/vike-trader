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
    rest_on(c, coid, SYMBOL);
}

/// [`rest`] on another symbol of the same venue.
fn rest_on(c: &mut CoreThread<RecordingClient>, coid: &str, symbol: &str) {
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: coid.into(),
            venue: VENUE.into(),
            symbol: symbol.into(),
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

// ---- `mass-cancel <venue> <symbol>`: a named symbol narrows the venue-side cancel ----------------

/// THE DEFECT: the engine's mass-cancel takes no symbol, so `mass-cancel bybit BTCUSDT` cancelled
/// every live order of the venue and used the symbol only to scope the core-held stores. The registry
/// orders cancelled are now the named symbol's; no symbol keeps the venue-wide cancel.
#[test]
fn a_mass_cancel_naming_a_symbol_cancels_only_that_symbols_registry_orders() {
    let mut c = core();
    rest(&mut c, "btc1");
    rest_on(&mut c, "eth1", "ETHUSDT");

    command(
        &mut c,
        OrderIntent::MassCancel {
            venue: Some(VENUE.into()),
            symbol: Some(SYMBOL.into()),
            account: None,
        },
    );

    assert_eq!(
        c.engine.client.cancels,
        vec!["btc1".to_string()],
        "only the named symbol's order is cancelled; ETHUSDT keeps resting"
    );

    // SENTINEL: the same two orders with no symbol named are both cancelled, as ever.
    let mut all = core();
    rest(&mut all, "btc1");
    rest_on(&mut all, "eth1", "ETHUSDT");
    command(
        &mut all,
        OrderIntent::MassCancel { venue: Some(VENUE.into()), symbol: None, account: None },
    );
    assert_eq!(all.engine.client.cancels, vec!["btc1".to_string(), "eth1".to_string()]);
}

/// A named symbol nothing rests on cancels nothing (it must not fall back to the whole venue).
#[test]
fn a_mass_cancel_naming_a_symbol_nothing_rests_on_cancels_nothing() {
    let mut c = core();
    rest(&mut c, "btc1");

    command(
        &mut c,
        OrderIntent::MassCancel {
            venue: Some(VENUE.into()),
            symbol: Some("SOLUSDT".into()),
            account: None,
        },
    );

    assert!(c.engine.client.cancels.is_empty(), "{:?}", c.engine.client.cancels);
}

/// PRESERVE: a STRATEGY's `mass_cancel()` also carries its mount's symbol (`drain_broker`), but its
/// documented meaning is "cancel ALL of this engine's live orders" and it keeps pulling the whole engine
/// (`EngineRoute::Mount`).
#[test]
fn a_strategys_mass_cancel_still_pulls_the_whole_engine() {
    let mut c = core();
    c.mount_engine = vec![0];
    rest(&mut c, "btc1");
    rest_on(&mut c, "eth1", "ETHUSDT");

    c.apply_intent_routed(
        OrderIntent::MassCancel {
            venue: Some(VENUE.into()),
            symbol: Some(SYMBOL.into()),
            account: None,
        },
        0,
        CancelIntent::Unspecified,
        EngineRoute::Mount(0),
    );

    assert_eq!(c.engine.client.cancels, vec!["btc1".to_string(), "eth1".to_string()]);
}

// ---- `recon cancel <id>` (decision 0196, rule (b)): one explicit, demo-only, operator-only cancel --

/// A core whose one account is a DEMO one, the only tier the verb works on (`engine_on` seeds `paper`).
fn demo_core() -> CoreThread<RecordingClient> {
    let mut engine = engine_on(VENUE, SYMBOL, RecordingClient::default());
    engine.set_tier(vike_model::AccountTier::Demo);
    core_of(engine, Vec::new(), CoreConfig::default())
}

fn cancel_row(c: &mut CoreThread<RecordingClient>, id: u64) {
    c.dispatch(Ingest::Command(Command::CancelReconOrder(id)));
}

/// The refusal lines of row `id`, which all start the same way.
fn refusals_of(c: &CoreThread<RecordingClient>, id: u64) -> Vec<String> {
    lines(c, &format!("RECON cancel #{id}: REFUSED, nothing sent: "))
}

/// FOUND: one cancel goes to the account's client for the coid the row carries, the registry order and
/// the other row are not touched, and nothing is folded or removed (the next pass clears the row).
#[test]
#[traced_test]
fn a_recon_cancel_sends_one_cancel_for_the_rows_coid_and_folds_nothing() {
    let mut c = demo_core();
    rest(&mut c, "r1");
    let rows = hold(&mut c, vec![venue_order("v-1", "b6lim"), venue_order("v-2", "b6lim2")]);
    assert_eq!(rows.len(), 2, "premise");
    let id = *rows
        .iter()
        .find(|id| c.recon_alerts[*id].identity.instance.contains("\"b6lim\""))
        .expect("the b6lim row");

    cancel_row(&mut c, id);

    assert_eq!(
        c.engine.client.cancel_intents,
        vec![("b6lim".to_string(), CancelIntent::Unspecified)],
        "ONE cancel, for the named row's coid; the registry order r1 and the b6lim2 row are untouched"
    );
    let said = lines(&c, &format!("RECON cancel #{id}: cancel SENT to bybit for coid b6lim"));
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(
        said[0].contains("did not cancel the order"),
        "says how to tell it failed: {}",
        said[0]
    );
    for row in &rows {
        assert!(c.recon_alerts.contains_key(row), "row #{row} is not removed here");
    }
    assert!(
        c.engine.registry.get("r1").is_some_and(|mo| mo.status.is_live()),
        "the registry order is untouched"
    );
    assert!(logs_contain("reconcile cancel SENT"), "a line on the confirm-verdict target");
}

/// An id no row carries (cleared by a later pass, or from before the restart) sends nothing.
#[test]
fn a_recon_cancel_of_an_unknown_id_is_refused() {
    let mut c = demo_core();

    cancel_row(&mut c, 12345);

    let said = refusals_of(&c, 12345);
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("no held row has that id"), "{}", said[0]);
    assert!(c.engine.client.cancels.is_empty());
}

/// `paper` has no venue order and `live` is the owner's call: both refused, with the tier in the words.
#[test]
fn a_recon_cancel_is_refused_off_a_demo_account() {
    for tier in [vike_model::AccountTier::Paper, vike_model::AccountTier::Live] {
        let mut c = core();
        c.engine.set_tier(tier);
        let rows = hold(&mut c, vec![venue_order("v-1", "b6lim")]);
        assert_eq!(rows.len(), 1, "{tier:?}: premise");

        cancel_row(&mut c, rows[0]);

        let said = refusals_of(&c, rows[0]);
        assert_eq!(said.len(), 1, "{tier:?}: {:?}", c.recent);
        assert!(said[0].contains(&format!("{} tier", tier.as_str())), "{}", said[0]);
        assert!(said[0].contains("demo account only"), "{}", said[0]);
        assert!(c.engine.client.cancels.is_empty(), "{tier:?}: nothing sent");
    }
}

/// A row with no coid (IG, FXCM and Polymarket report none; a UI order on Hyperliquid has none either)
/// carries no id the venue can cancel by: refused, nothing sent.
#[test]
fn a_recon_cancel_of_a_row_without_a_usable_id_is_refused() {
    let mut c = demo_core();
    let mut none = venue_order("v-1", "x");
    none.client_order_id = None;
    let rows = hold(&mut c, vec![none]);
    assert_eq!(rows.len(), 1, "premise");

    cancel_row(&mut c, rows[0]);

    let said = refusals_of(&c, rows[0]);
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("this row carries no id the venue can cancel by"), "{}", said[0]);
    assert!(c.engine.client.cancels.is_empty(), "nothing sent");
}

/// Hyperliquid's own 128-bit hex `cloid` (`0x` and 32 hex digits, the digest of our coid) is the id such
/// a row prints, and it is SENT AS PRINTED: this core names no venue and does not second-guess the
/// shape, the bridge reads it back as the venue's `cloid` (and refuses on its own when the account does
/// not list it). The registry order beside it is untouched.
#[test]
fn a_recon_cancel_of_a_hex_shaped_id_is_sent_as_printed() {
    const HEX: &str = "0xb395aaaab395aaaab395aaaab395aaaa";
    let mut c = demo_core();
    rest(&mut c, "r1");
    let rows = hold(&mut c, vec![venue_order("v-2", HEX)]);
    assert_eq!(rows.len(), 1, "premise");

    cancel_row(&mut c, rows[0]);

    assert_eq!(
        c.engine.client.cancel_intents,
        vec![(HEX.to_string(), CancelIntent::Unspecified)],
        "ONE cancel, for the id exactly as the row prints it"
    );
    let said =
        lines(&c, &format!("RECON cancel #{}: cancel SENT to bybit for coid {HEX}", rows[0]));
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(refusals_of(&c, rows[0]).is_empty(), "{:?}", c.recent);
}

/// The verb is for orders the registry does NOT hold: a row whose id now rests in a registry is
/// refused, so this path can never cancel a registry order behind the engine's back.
#[test]
fn a_recon_cancel_never_touches_an_order_the_registry_holds() {
    let mut c = demo_core();
    let rows = hold(&mut c, vec![venue_order("v-1", "b6lim")]);
    assert_eq!(rows.len(), 1, "premise");
    rest(&mut c, "b6lim");

    cancel_row(&mut c, rows[0]);

    let said = refusals_of(&c, rows[0]);
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("order registry holds b6lim"), "{}", said[0]);
    assert!(c.engine.client.cancels.is_empty(), "{:?}", c.engine.client.cancels);
}

/// The verb cancels the venue order of an `UnknownOrder` row and nothing else: a `MissingFill` row's
/// id is refused (`recon confirm` resolves that kind).
#[test]
fn a_recon_cancel_of_a_row_of_another_kind_is_refused() {
    let mut c = demo_core();
    let mut p = pass(Vec::new());
    p.fills = vec![vike_model::FillReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        trade_id: "t1".into(),
        venue_order_id: "v9".into(),
        client_order_id: None,
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        ts: 5,
    }];
    c.reconcile_reports(p);
    let id = c
        .recon_alerts
        .iter()
        .find(|(_, h)| h.kind == DivergenceKind::MissingFill)
        .map(|(&id, _)| id)
        .expect("premise: a held MissingFill row");

    cancel_row(&mut c, id);

    let said = refusals_of(&c, id);
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(said[0].contains("MissingFill row"), "{}", said[0]);
    assert!(c.engine.client.cancels.is_empty());
}

/// The helpers `dispatch` reaches only on a miss stay out of line (`#[inline(never)]`, as
/// `crates/vike-core/CLAUDE.md` asks of every cold helper `dispatch` calls); inlined, their
/// `format!`s and iterator chains would grow the function the core-hop gate measures. This pins the
/// attribute; what it buys is measured by the `latency` job, not here.
#[test]
fn the_cold_helpers_stay_out_of_line_of_dispatch() {
    let src = include_str!("../refusals.rs");
    let all: Vec<&str> = src.lines().collect();
    for name in [
        "report_unheld_cancel",
        "report_unreached_rows",
        "unheld_cancel_reason",
        "cancel_recon_order",
        "refuse_recon_cancel",
        "announce_recovered_orders",
    ] {
        let at = all
            .iter()
            .position(|l| l.contains(&format!("fn {name}(")))
            .unwrap_or_else(|| panic!("{name} not found in refusals.rs"));
        assert_eq!(all[at - 1].trim(), "#[inline(never)]", "{name} lost its attribute");
    }
}

// ---- `recon cancel` tells the client WHERE the row's order rests, before it cancels it -----------
//
// The defect: a per-symbol router bridge (binance, bybit, okx: `PerSymbolRest` over `CoidSymbols`) learns a
// foreign order's symbol only from its own submit or from `adopt_restored`. `recon cancel` named a coid the
// registry does not hold, so the router never heard it: on an admitted symbol that is NOT the mounted one the
// cancel went to the MOUNTED symbol, the venue answered "unknown order", the bridge swallowed that as "already
// gone", and the verb said SENT while the order kept resting. The fix teaches the client the row's symbol first
// (`row_symbol_is_taught` says for which venues), so the cancel lands where the order rests.

use std::collections::{HashMap, HashSet, VecDeque};
use vike_exec::ExecutionClient;
use vike_model::events::{Event, OrderCanceled};

use super::refusals::{held_row_symbol, row_symbol_is_taught, symbol_before_coid};

const ETH: &str = "ETHUSDT";

/// What a per-symbol router bridge does with a cancel, over a fake venue book. The router knows a coid's
/// symbol only from `adopt_restored` here (a submit would fill it too, and this core submitted nothing);
/// every other coid goes to the MOUNTED symbol, where the venue's "unknown order" is swallowed as "already
/// gone" and still reported as `OrderCanceled` (`crates/bridges/binance/src/per_symbol.rs` and its bybit and
/// okx twins; measured by the bridges' own repro tests).
struct RouterClient {
    mounted: String,
    taught: HashMap<String, String>,
    /// The fake venue's resting orders, `(symbol, coid)`.
    book: Vec<(String, String)>,
    /// Every cancel as the venue received it, `(symbol, coid, found)`.
    wire: Vec<(String, String, bool)>,
    events: VecDeque<Event>,
}

impl RouterClient {
    fn new(mounted: &str, book: &[(&str, &str)]) -> Self {
        Self {
            mounted: mounted.into(),
            taught: HashMap::new(),
            book: book.iter().map(|(s, c)| (s.to_string(), c.to_string())).collect(),
            wire: Vec::new(),
            events: VecDeque::new(),
        }
    }
}

impl ExecutionClient for RouterClient {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, client_order_id: &str) {
        let symbol = self.taught.get(client_order_id).unwrap_or(&self.mounted).clone();
        let at = self.book.iter().position(|(s, c)| *s == symbol && c == client_order_id);
        self.wire.push((symbol, client_order_id.to_string(), at.is_some()));
        if let Some(i) = at {
            self.book.remove(i);
        }
        // Found or swallowed as gone: the lane reports the same `OrderCanceled`.
        self.events.push_back(Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: "venue".to_string().into(),
            ts: 0,
        }));
    }
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        for (coid, symbol) in orders {
            self.taught.insert(coid.clone(), symbol.clone());
        }
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// A DEMO core over one engine of `venue` mounted on `symbol`.
fn demo_core_of<C: ExecutionClient>(venue: &str, symbol: &str, client: C) -> CoreThread<C> {
    let mut engine = engine_on(venue, symbol, client);
    engine.set_tier(vike_model::AccountTier::Demo);
    core_of(engine, Vec::new(), CoreConfig::default())
}

/// A resting order of `venue` on `symbol`, as that venue's open-order read reports it.
fn resting(venue: &str, symbol: &str, venue_id: &str, coid: &str) -> OrderStatusReport {
    OrderStatusReport { venue: venue.into(), symbol: symbol.into(), ..venue_order(venue_id, coid) }
}

/// One quarantine pass of `venue` over `orders`; the id of the one held `UnknownOrder` row it raises.
fn hold_row<C: ExecutionClient>(
    c: &mut CoreThread<C>,
    venue: &str,
    orders: Vec<OrderStatusReport>,
) -> u64 {
    let mut p = pass(orders);
    p.venue = venue.into();
    c.reconcile_reports(p);
    let rows: Vec<u64> = c
        .recon_alerts
        .iter()
        .filter(|(_, h)| h.kind == DivergenceKind::UnknownOrder)
        .map(|(&id, _)| id)
        .collect();
    assert_eq!(rows.len(), 1, "premise: ONE held UnknownOrder row, got {rows:?}");
    rows[0]
}

fn recon_cancel<C: ExecutionClient>(c: &mut CoreThread<C>, id: u64) {
    c.dispatch(Ingest::Command(Command::CancelReconOrder(id)));
}

/// The ring lines that start with `prefix` (the generic twin of [`lines`]).
fn ring<C: ExecutionClient>(c: &CoreThread<C>, prefix: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.starts_with(prefix)).map(|l| l.to_string()).collect()
}

/// THE DEFECT, end to end: a row on an admitted symbol that is not the mounted one. The cancel must reach
/// the row's own symbol and the venue must lose the order. RED before the fix: the router sent it to the
/// mounted symbol, the venue found nothing there, and the order kept resting under a SENT note.
#[test]
fn a_recon_cancel_of_a_row_on_another_symbol_reaches_that_symbol() {
    let mut c = demo_core_of(VENUE, SYMBOL, RouterClient::new(SYMBOL, &[(ETH, "b6eth")]));
    let id = hold_row(&mut c, VENUE, vec![resting(VENUE, ETH, "v-1", "b6eth")]);

    recon_cancel(&mut c, id);

    assert_eq!(
        c.engine.client.wire,
        vec![(ETH.to_string(), "b6eth".to_string(), true)],
        "the venue got the cancel on the row's own symbol and found the order there"
    );
    assert!(c.engine.client.book.is_empty(), "the order is gone at the venue");
    assert_eq!(
        ring(&c, &format!("RECON cancel #{id}: cancel SENT to bybit for coid b6eth")).len(),
        1
    );
    assert!(
        c.recon_alerts.contains_key(&id),
        "the row stays until a later pass no longer raises it"
    );
}

/// PRESERVE: a venue "not found" on the RIGHT symbol (the order filled or went away between the pass and
/// the cancel) still reads as gone, exactly as the bridges read it: the cancel was SENT, the lane's
/// `OrderCanceled` reaches the core, and the core folds nothing for a coid no registry holds.
#[test]
fn a_venue_not_found_on_the_rows_symbol_still_reads_as_gone_and_folds_nothing() {
    let mut c = demo_core_of(VENUE, SYMBOL, RouterClient::new(SYMBOL, &[(ETH, "b6eth")]));
    let id = hold_row(&mut c, VENUE, vec![resting(VENUE, ETH, "v-1", "b6eth")]);
    c.engine.client.book.clear(); // it filled after the pass

    recon_cancel(&mut c, id);

    assert_eq!(
        c.engine.client.wire,
        vec![(ETH.to_string(), "b6eth".to_string(), false)],
        "the cancel reached the row's symbol and the venue answered not-found there"
    );
    assert!(
        c.engine.client.events.is_empty(),
        "the lane's OrderCanceled was pumped through the core"
    );
    assert_eq!(
        c.engine.dropped_unknown_coid, 1,
        "dropped as an event of an unknown coid, as before"
    );
    assert!(c.engine.registry.is_empty(), "nothing was registered or folded");
    assert_eq!(ring(&c, &format!("RECON cancel #{id}: cancel SENT")).len(), 1, "{:?}", c.recent);
    assert!(c.recon_alerts.contains_key(&id));
}

/// PRESERVE: a row on the MOUNTED symbol cancels exactly as before (the teaching names the symbol the
/// router would have picked anyway).
#[test]
fn a_recon_cancel_of_a_row_on_the_mounted_symbol_is_unchanged() {
    let mut c = demo_core_of(VENUE, SYMBOL, RouterClient::new(SYMBOL, &[(SYMBOL, "b6btc")]));
    let id = hold_row(&mut c, VENUE, vec![resting(VENUE, SYMBOL, "v-1", "b6btc")]);

    recon_cancel(&mut c, id);

    assert_eq!(
        c.engine.client.wire,
        vec![(SYMBOL.to_string(), "b6btc".to_string(), true)],
        "one cancel, on the mounted symbol, found"
    );
    assert!(c.engine.client.book.is_empty());
    assert_eq!(ring(&c, &format!("RECON cancel #{id}: cancel SENT")).len(), 1);
    assert!(refusals_of_any(&c).is_empty(), "{:?}", c.recent);
}

fn refusals_of_any<C: ExecutionClient>(c: &CoreThread<C>) -> Vec<String> {
    ring(c, "RECON cancel #").into_iter().filter(|l| l.contains("REFUSED")).collect()
}

/// PRESERVE (the guard that found the regression): Hyperliquid's row prints the venue's own hashed `cloid`,
/// which `adopt_restored` would seed as if it were a coid and make the bridge hash a second time instead of
/// sending the wire path `recon cancel` was proven on. The pair is NEVER taught there; the cancel is sent as
/// printed, exactly as before. The same for the venues whose restore is off: ibkr (a taught order's fills
/// are emitted) and oanda here, and every other one by `the_teaching_reaches_...` below (the two tables).
#[test]
fn a_venue_that_hashes_the_id_or_does_not_restore_is_never_taught_the_row() {
    const HEX: &str = "0xb395aaaab395aaaab395aaaab395aaaa";
    for (venue, symbol, coid) in [
        ("hyperliquid", "BTC", HEX),
        ("ibkr", "AAPL.SMART.USD", "b6ibkrlim"),
        ("oanda", "EUR_USD", "b6oandalim"),
    ] {
        let mut c = demo_core_of(venue, symbol, RecordingClient::default());
        let id = hold_row(&mut c, venue, vec![resting(venue, symbol, "v-1", coid)]);

        recon_cancel(&mut c, id);

        assert!(c.engine.client.adopted.is_empty(), "{venue}: nothing is taught");
        assert_eq!(
            c.engine.client.cancel_intents,
            vec![(coid.to_string(), CancelIntent::Unspecified)],
            "{venue}: the cancel is sent as printed, as before"
        );
    }
}

/// The venues the teaching DOES reach, by the two tables it reads (so a table flip moves this test, not a
/// hidden list): the restore is open and the report echoes our coid.
#[test]
fn the_teaching_reaches_the_venues_whose_restore_is_open_and_whose_reports_echo_the_coid() {
    let mut taught: Vec<&str> =
        vike_model::VENUES.iter().copied().filter(|v| row_symbol_is_taught(v)).collect();
    taught.sort_unstable();
    assert_eq!(taught, vec!["binance", "bybit", "deribit", "okx"], "{taught:?}");
}

/// The text reader against the REAL raise path: the symbol a held row's identity prints, whatever its
/// shape, and the `describe` text of the divergence (the fallback), read back whole.
#[test]
fn the_row_symbol_reads_back_what_the_real_divergence_prints() {
    for symbol in [ETH, "EUR/USD", "AAPL.SMART.USD", "HYPE/USDC", "BTCUSDT.P"] {
        let mut c = core();
        let id = hold_row(&mut c, VENUE, vec![resting(VENUE, symbol, "v-1", "b6x")]);
        assert_eq!(held_row_symbol(&c.recon_alerts[&id]), Some(symbol), "identity of the held row");

        let report = resting(VENUE, symbol, "v-1", "b6x");
        let d = vike_exec::recon::Divergence::UnknownOrder(report);
        assert_eq!(symbol_before_coid(&d.describe()), Some(symbol), "describe()");
        assert_eq!(symbol_before_coid(&d.identity_detail()), Some(symbol), "identity_detail()");
    }
    // No coid printed, no symbol read (the symbol sits in the same sentence as the coid).
    assert_eq!(
        symbol_before_coid("unknown venue order 77 (bybit ETHUSDT) status Accepted filled 0/1"),
        None
    );
    let mut none = resting(VENUE, ETH, "v-1", "x");
    none.client_order_id = None;
    let d = vike_exec::recon::Divergence::UnknownOrder(none);
    assert_eq!(symbol_before_coid(&d.describe()), None, "coid=None");
}

/// The journal-recovery twin: an operator's confirm of a `JournalDivergence` re-registers the lost order
/// into the registry (`reregister_orders`) and now tells the client where it rests, as the restore does.
#[test]
fn a_confirmed_journal_divergence_tells_the_client_where_each_order_rests() {
    for (venue, symbol, taught) in [(VENUE, ETH, true), ("hyperliquid", "BTC", false)] {
        let config = CoreConfig {
            journal_view_provider: Some(Box::new(|_venue: &str| vike_exec::recon::JournalView {
                seen_trade_ids: HashSet::new(),
                // the journal says c-lost is still LIVE; this core has never heard of it
                orders: std::iter::once(("c-lost".to_string(), OrderStatus::Accepted)).collect(),
            })),
            ..CoreConfig::default()
        };
        let mut engine = engine_on(venue, SYMBOL, RecordingClient::default());
        engine.set_tier(vike_model::AccountTier::Demo);
        let mut c = core_of(engine, Vec::new(), config);
        let mut p = pass(vec![resting(venue, symbol, "v-lost", "c-lost")]);
        p.venue = venue.into();
        c.reconcile_reports(p);
        let id = c
            .recon_alerts
            .iter()
            .find(|(_, h)| h.kind == DivergenceKind::JournalDivergence)
            .map(|(&id, _)| id)
            .unwrap_or_else(|| panic!("{venue}: premise: a held JournalDivergence row"));

        c.confirm_recon(id);

        assert!(c.engine.registry.contains_key("c-lost"), "{venue}: re-registered");
        let expected: Vec<(String, String)> =
            if taught { vec![("c-lost".to_string(), symbol.to_string())] } else { Vec::new() };
        assert_eq!(c.engine.client.adopted, expected, "{venue}");
    }
}
