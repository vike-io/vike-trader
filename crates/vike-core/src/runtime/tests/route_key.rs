//! Routing by `Engine::route_key` — the gate over the split that made two accounts of one
//! venue expressible in one process.
//!
//! # What was wrong
//!
//! [`CoreThread::engine_index_for_route_key`] used to compare against `Engine::venue`, the
//! same field every per-venue capability table is keyed on (`vike_venues::capabilities_for`,
//! `amend_semantics`, `fee_schedule_for`, …). One field, two incompatible jobs — so a second
//! account of an exchange had no spelling:
//!
//! * label both engines `"binance"` and this function returns the FIRST match forever. The second
//!   engine is unreachable for fills, both accounts fold into ONE book, and reconcile compounds it:
//!   under `hybrid` `PositionDrift` auto-applies, so each pass rewrites the local position onto
//!   whichever account answered last.
//! * label the second `"binance#2"` and routing works while every capability lookup misses — the
//!   trap `crates/vike-execution/tests/engine/route_key.rs` gates from the other side.
//!
//! # …and the two ROUND TRIPS the split left open
//!
//! Splitting the engine's field did not split the strings that reach it. Two chains carried a
//! CANONICAL venue out of the core and handed it back as a ROUTING key, and both are gated below:
//!
//! * **The reconcile round trip.** `ReconcileReports` carried one venue string;
//!   `CoreThread::reconcile_reports` routed on it, then STORED it on `ReconcileHeldAlert` and routed on
//!   it AGAIN in `confirm_reconcile` when an operator approved the held events. The payload now carries
//!   `route_key` beside `venue`, the held record keeps both, and the sections below drive each leg
//!   with the two facts different.
//! * **The order-payload double load.** `OrderRequest::venue` is what `engine_index_for_route_key`
//!   routes on AND what `vike_venues::preflight_order` selected the capability row by — and a
//!   non-roster string does not fail closed there, it returns `Ok(())`. `CoreThread::capabilities_venue`
//!   now asks the ROUTED ENGINE for its canonical venue instead of asking the payload.
//!
//! # White-box, and why
//!
//! `engine_index_for_route_key`, `capabilities_venue`, `route_event`, `publish_to`, `reconcile_reports`,
//! `confirm_reconcile`, `apply_intent` and the `reconcile_alerts` store are all private to this module, and
//! they are precisely what is under test — composing `route_event` with `publish_to` is exactly
//! what the `Ingest::Event` dispatch arm does inline, and `reconcile_reports`/`confirm_reconcile` are
//! what the `Command::ReconcileReports`/`ConfirmReconcile` arms call. The `use super::*`
//! sibling-test-module idiom of `crates/vike-core/src/runtime/tests/safe_state.rs`/
//! `crates/vike-core/src/runtime/tests/multi_mount.rs`.

use super::*;
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, RiskGate};
use vike_model::RiskLimits;
use vike_model::events::{FillEvent, TradeId};

// The runtime's shared white-box assembly; every core here passes `CoreConfig::default()`.
use crate::runtime::test_support::{core_of, held_at};

/// The canonical venue BOTH engines below declare. A roster id, so every capability table resolves
/// its real row for either engine.
const CANON: &str = "binance";
/// The primary account's routing key — equal to [`CANON`], i.e. the default `Engine::new`
/// produces and the shape every mount in this workspace builds.
const ACCOUNT_A: &str = "binance";
/// The SECOND account's routing key. Not a roster id; that is the point.
const ACCOUNT_B: &str = "binance-sub2";

const SYMBOL: &str = "BTCUSDT";

/// One engine whose canonical venue is `venue` and whose routing key is `route_key`.
///
/// ⚠ The `Account` is seeded with `route_key`, not `venue`, and that is not a shortcut — it is a
/// second seam this split does NOT close, recorded here because the test would otherwise hide it.
/// `Account::apply_fill` opens by REFUSING any fill whose `venue` is not `self.venue`
/// (`FillFold::ForeignVenue`), so a fill can only fold into an account whose venue string it carries. Routing a fill to the right one of two
/// same-venue engines therefore needs the fill to be DISTINGUISHABLE, and today the only field on
/// the payload that can distinguish it is the one that assert compares. Wiring a real second
/// account has to give `FillEvent` a route key of its own (or route it by order client id, the way
/// `order_client_id_venue` already routes order-lifecycle replies, which carry no venue at all); until then
/// this is how the two-account shape is expressible at all, and pinning it here is what makes the
/// gap visible instead of theoretical.
fn engine(venue: &str, route_key: &str, symbol: &str) -> Engine<RecordingClient> {
    engine_with_account_venue(venue, route_key, route_key, symbol)
}

/// [`engine`] with the `Account`'s venue label chosen SEPARATELY from the routing key.
///
/// The fill-routing tests need it to be the route key (see [`engine`]'s note). The RECONCILE tests
/// need the opposite: a reconcile pass's synthesized fills carry the venue off the venue's own
/// FILL REPORT, which an adapter mints canonically, so they only fold into an account labelled
/// with the canonical venue. Both engines can carry that label at once precisely because reconcile
/// does NOT route by the fill — `reconcile_reports` resolves the engine ONCE from the pass's route
/// key and then publishes straight to it, which is the whole hop under test.
fn engine_with_account_venue(
    venue: &str,
    route_key: &str,
    account_venue: &str,
    symbol: &str,
) -> Engine<RecordingClient> {
    let mut e = Engine::new(
        Account::new(1.0, account_venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        symbol,
    );
    e.route_key = route_key.to_string();
    e
}

/// The two-account core: one canonical venue, two routing keys.
fn two_accounts_of_one_venue() -> CoreThread<RecordingClient> {
    core_of(
        engine(CANON, ACCOUNT_A, SYMBOL),
        vec![(0.0, engine(CANON, ACCOUNT_B, SYMBOL))],
        CoreConfig::default(),
    )
}

fn fill(route_key: &str, trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        order_client_id: String::new(),
        venue: route_key.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(100.0),
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// Signed `BOTH` size held by the engine at routing index `index`, under `key`.
fn held(core: &CoreThread<RecordingClient>, index: usize, key: &str) -> f64 {
    held_at(core, index, key, SYMBOL)
}

/// The second account's own symbol — distinct from [`SYMBOL`], so the SYMBOL lane below has an
/// unambiguous answer to give. Two accounts sharing one symbol is now a legal mount; it simply
/// routes by order client id instead, which the fill-lane test below covers.
const SYMBOL_B: &str = "ETHUSDT";

/// A fill as a VENUE actually emits one: tagged with the canonical venue, and with the symbol that
/// says which book it belongs to.
fn wire_fill(symbol: &str, trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        order_client_id: String::new(),
        venue: CANON.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(100.0),
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// Two accounts of one exchange, each on its OWN symbol — the configuration in which the SYMBOL
/// lane has an unambiguous answer. (`vike_tradehub_core::make_engine_accounts` will mount two accounts on
/// ONE symbol too, now that the collision rule is gone; that configuration routes by order client id, and
/// `a_shared_symbol_routes_by_order_client_id_not_by_first_match` below is its gate.)
fn two_accounts_on_two_symbols() -> CoreThread<RecordingClient> {
    core_of(
        // ⚠ Both `Account`s are seeded with the CANONICAL venue, which is what
        // `vike_tradehub_core::make_engine_for_account` does: it passes `venue` to `Account::new` and
        // decorates only `route_key`. `Account::apply_fill` refuses a fill whose venue is not its
        // own, so seeding a route key here would make the harness reject the very wire payload it
        // exists to route. The two books stay distinct because their SYMBOLS differ.
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL_B))],
        CoreConfig::default(),
    )
}

// ---- the venue's quote reaches every engine that was admitted the symbol (decision 0174) --------

const ETH: &str = "ETHUSDT";
const ALT: &str = "binance#ALT";
const BOB: &str = "binance#BOB";

fn admission(route_key: &str) -> vike_execution::SymbolAdmission {
    vike_execution::SymbolAdmission {
        route_key: route_key.into(),
        symbol: ETH.into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.01),
            lot_size: Some(0.0001),
            ..vike_model::SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    }
}

fn admit(core: &mut CoreThread<RecordingClient>, route_key: &str) {
    core.dispatch(Ingest::Command(Command::AdmitSymbol(Box::new(admission(route_key)))));
}

fn quote(symbol: &str, px: f64) -> Ingest {
    Ingest::Quote(Box::new(vike_execution::QuoteUpdate {
        venue: CANON.into(),
        symbol: symbol.into(),
        quote: vike_marketdata::TickQuote {
            timestamp: 1,
            local_timestamp: 1,
            bid: px,
            ask: px,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: symbol.into(),
        },
    }))
}

fn priced(core: &CoreThread<RecordingClient>, index: usize, symbol: &str) -> bool {
    core.engine(index).price_board.cell(CANON, symbol).is_some()
}

/// binance's DEFAULT account at engine 0 and two LABELLED accounts of `venue` (`a` at 1, `b` at 2),
/// all on [`SYMBOL`]: `(CANON, ALT, BOB)` is the venue's default and two labelled ones; `(BYBIT,
/// DEMO_KEY, LIVE_KEY)` (`payload_capability`) is a venue with TWO labelled accounts and no default
/// one, which no unlabelled event could belong to.
fn default_and_two_labelled(venue: &str, a: &str, b: &str) -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![
            (0.0, engine_with_account_venue(venue, a, venue, SYMBOL)),
            (0.0, engine_with_account_venue(venue, b, venue, SYMBOL)),
        ],
        CoreConfig::default(),
    )
}

#[test]
fn a_core_that_admitted_nothing_into_a_labelled_account_fans_out_to_nobody() {
    let mut core = default_and_two_labelled(CANON, ALT, BOB);
    assert!(core.admitted_fanout.is_empty(), "nothing admitted yet");
    // The DEFAULT account's admission is the existing route's business, never the list's.
    admit(&mut core, ACCOUNT_A);
    assert!(core.admitted_fanout.is_empty(), "{:?}", core.admitted_fanout);
    core.dispatch(quote(ETH, 3000.0));
    assert!(priced(&core, 0, ETH), "the default account is priced by the existing route");
    assert!(
        !priced(&core, 1, ETH) && !priced(&core, 2, ETH),
        "labelled accounts never admitted it"
    );
}

#[test]
fn an_admission_into_a_labelled_account_prices_that_account_and_only_it() {
    let mut core = default_and_two_labelled(CANON, ALT, BOB);
    admit(&mut core, ALT);
    assert_eq!(core.admitted_fanout, vec![1], "ALT is listed, the default and BOB are not");
    core.dispatch(quote(ETH, 3000.0));
    assert!(priced(&core, 1, ETH), "ALT holds ETH and is now priced");
    assert!(!priced(&core, 0, ETH), "the default account never admitted ETH");
    assert!(!priced(&core, 2, ETH), "BOB never admitted ETH");
    // The default account's mounted symbol is delivered once, by the existing route. A listed engine
    // is handed every symbol it accepts (its own mounted one too: the same venue book), an unlisted
    // one nothing it did not already get.
    core.dispatch(quote(SYMBOL, 100.0));
    assert!(priced(&core, 0, SYMBOL) && priced(&core, 1, SYMBOL) && !priced(&core, 2, SYMBOL));
}

/// A quote on a symbol only a LABELLED account holds writes that account's board and nothing else:
/// the default engine does not accept it and no mount hears it. The idle publish runs only when the
/// core is dirty, so `fan_out_quote` marks it when it wrote a price, and only then.
#[test]
fn a_fanned_out_quote_marks_the_core_for_a_publish_and_a_quote_nobody_holds_does_not() {
    let mut core = default_and_two_labelled(CANON, ALT, BOB);
    admit(&mut core, ALT);
    core.dirty = false;
    core.dispatch(quote("XRPUSDT", 1.0));
    assert!(!core.dirty, "no engine accepts it: nothing was written, so nothing to publish");
    core.dispatch(quote(ETH, 3000.0));
    assert!(priced(&core, 1, ETH) && !priced(&core, 0, ETH), "PREMISE: only ALT holds ETH");
    assert!(core.dirty, "ALT's board moved and nothing else dirtied the core");
}

fn trade(symbol: &str, px: f64) -> Ingest {
    Ingest::Trade(Box::new(vike_execution::TradeUpdate {
        venue: CANON.into(),
        symbol: symbol.into(),
        trade: vike_marketdata::TickTrade {
            timestamp: 1,
            local_timestamp: 1,
            price: px,
            size: 1.0,
            is_buyer_maker: false,
            symbol: symbol.into(),
        },
    }))
}

/// **A DECISION, pinned: a fanned-out TRADE prices the labelled account's board and does NOT mark the
/// core for a publish** (v0.1.80's behaviour; `fan_out_quote`, which does, is the test above). Marking it
/// was built and measured on the multi-currency branch: with three labelled accounts that each hold a
/// position and eight resting orders in the symbol (`runtime_latency`'s `trade-hop-labelled-held-x3`) a
/// print then cost one whole publish and the core-hop p99 rose by about 4.6 us (IQR 0.6 to 5.1 us)
/// against a 10 us `P99_BUDGET_NS`, p50 unmoved. A trade-only price move of a held position therefore
/// waits for the next event that publishes (a quote, a fill, the 16 ms cadence). To take that cost for
/// prompter valuations, set `dirty` in `fan_out_trade` when an engine holding a position in the symbol
/// was written, and re-run that variant.
#[test]
fn a_fanned_out_trade_prices_the_board_and_does_not_mark_the_core() {
    let mut core = default_and_two_labelled(CANON, ALT, BOB);
    admit(&mut core, ALT);
    core.engine_mut(1).account.positions.insert(
        (CANON.into(), ETH.into(), vike_model::events::PositionSide::Both),
        vike_execution::PositionEntry { size: 1.0, average_price: 2900.0, ..Default::default() },
    );
    core.dirty = false;
    core.dispatch(trade(ETH, 3000.0));
    assert!(priced(&core, 1, ETH) && !priced(&core, 0, ETH), "PREMISE: ALT's board moved");
    assert!(!core.dirty, "…and the trade lane leaves the publish to the next event (see the doc)");
}

#[test]
fn two_labelled_accounts_admitting_one_symbol_are_both_listed_and_both_priced() {
    let mut core = default_and_two_labelled(CANON, ALT, BOB);
    admit(&mut core, BOB);
    admit(&mut core, ALT);
    admit(&mut core, ALT); // a re-sent admission refreshes; it must not list the engine twice
    assert_eq!(core.admitted_fanout, vec![1, 2], "routing-index order, each once");
    core.dispatch(quote(ETH, 3000.0));
    assert!(priced(&core, 1, ETH) && priced(&core, 2, ETH));
}

#[test]
fn a_trade_and_a_book_price_a_labelled_account_too() {
    let mut core = default_and_two_labelled(CANON, ALT, BOB);
    admit(&mut core, ALT);
    core.dispatch(Ingest::Trade(Box::new(vike_execution::TradeUpdate {
        venue: CANON.into(),
        symbol: ETH.into(),
        trade: vike_marketdata::TickTrade {
            timestamp: 1,
            local_timestamp: 1,
            price: 3000.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: ETH.into(),
        },
    })));
    assert!(priced(&core, 1, ETH), "a trade print");
    let mut core = default_and_two_labelled(CANON, ALT, BOB);
    admit(&mut core, ALT);
    let mut book = vike_marketdata::L2Book::new(0.01);
    book.apply_snapshot(
        1,
        &[vike_marketdata::L2BookLevel { price: 2999.0, qty: 1.0 }],
        &[vike_marketdata::L2BookLevel { price: 3001.0, qty: 1.0 }],
    );
    core.dispatch(Ingest::Book(Box::new(vike_execution::BookUpdate {
        venue: CANON.into(),
        symbol: ETH.into(),
        book: std::sync::Arc::new(book),
    })));
    assert!(priced(&core, 1, ETH), "a book's top");
}

/// A restart (or a replay from a base `Snapshot`) hands the core engines that ALREADY hold their
/// admissions: the list is rebuilt at assembly from the engines' folded state, with no command.
#[test]
fn an_admission_the_engines_were_restored_with_is_listed_at_assembly() {
    let mut alt = engine_with_account_venue(CANON, ALT, CANON, SYMBOL);
    assert_eq!(alt.admit_symbol(&admission(ALT)), vike_execution::AdmitOutcome::Admitted);
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, alt)],
        CoreConfig::default(),
    );
    assert_eq!(core.admitted_fanout, vec![1]);
}

/// A leg DECLARED on a labelled engine (an `extra_symbols` entry with no on-demand grid row) keeps
/// the lanes it always had: the fan-out is for admissions, not a change of behaviour for legs.
#[test]
fn a_declared_leg_on_a_labelled_engine_is_not_fanned_out() {
    let mut alt = engine_with_account_venue(CANON, ALT, CANON, SYMBOL);
    alt.extra_symbols.push(ETH.to_string());
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, alt)],
        CoreConfig::default(),
    );
    assert!(core.admitted_fanout.is_empty());
    core.dispatch(quote(ETH, 3000.0));
    assert!(!priced(&core, 1, ETH));
}

// ---- a labelled engine's OWN mounted symbol: the two paths today, pinned (decision 0208) ----------
//
// The venue's default account is mounted on the spot symbol and a labelled one (the Binance lane twin's
// shape) on a symbol the default engine does not accept. These are CHARACTERISATION tests of the base:
// they say what the core does with that symbol today, so a change that routes the ticks to the
// labelled engine (the "proper fix" of decisions 0184 item 7, 0197 and 0198, which decision 0208
// weighs and does not build) has to break them on purpose and answer for each line.

const PERP: &str = "BTCUSDT.P";
const TWIN: &str = "binance#PERP";

fn default_and_a_twin() -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, TWIN, CANON, PERP))],
        CoreConfig::default(),
    )
}

/// **PATH ONE, the tick: a quote, a trade and a book of a labelled engine's own mounted symbol price NO
/// engine.** The route reaches the venue's default engine (which does not accept the symbol) and
/// `admitted_fanout` lists only engines holding an ON-DEMAND admission, so the twin is not on it. Nothing
/// is written and nothing is marked for a publish: the fold does a lookup and nothing else for it.
#[test]
fn a_tick_of_a_labelled_engines_own_symbol_prices_no_board_and_dirties_nothing() {
    let mut core = default_and_a_twin();
    assert!(core.admitted_fanout.is_empty(), "the twin holds no on-demand admission");
    assert!(core.multi_account, "PREMISE: two engines of one exchange");
    core.dirty = false;
    core.dispatch(quote(PERP, 74_300.0));
    core.dispatch(trade(PERP, 74_300.0));
    let mut book = vike_marketdata::L2Book::new(0.5);
    book.apply_snapshot(
        1,
        &[vike_marketdata::L2BookLevel { price: 74_299.0, qty: 1.0 }],
        &[vike_marketdata::L2BookLevel { price: 74_301.0, qty: 1.0 }],
    );
    core.dispatch(Ingest::Book(Box::new(vike_execution::BookUpdate {
        venue: CANON.into(),
        symbol: PERP.into(),
        book: std::sync::Arc::new(book),
    })));
    assert!(!priced(&core, 1, PERP), "the twin's board never hears its own symbol's ticks");
    assert!(!priced(&core, 0, PERP), "…and neither does the default engine's");
    assert!(!core.dirty, "a tick that writes no board marks nothing for a publish");
}

/// **PATH TWO, the mark: a REAL venue MARK of the same symbol lands in TWO engines.** `drain_market` files
/// it on the venue's DEFAULT engine first (board mark slot AND the account mark scalar, which is what
/// `CoreSnapshot::marks` and `last_mark` read: engine 0's), then `mirror_venue_price` files it on every
/// other engine of the exchange (the twin's board and account mark), and marks the core for a publish. The
/// board's rung one outranks the quote and trade rungs, so a venue mark present is the price the twin is
/// valued at (decision 0221 leaves it alone). The node's price tap no longer feeds this lane with a mid: its
/// top of book is PATH THREE, below (owner, 2026-11-15).
#[test]
fn a_mark_of_a_labelled_engines_own_symbol_prices_the_twin_and_the_default_engines_published_slot()
{
    let mut core = default_and_a_twin();
    core.dirty = false;
    core.market.state.lock().unwrap().slots.insert(
        (CANON.to_string(), PERP.to_string()),
        vike_execution::MarketTick {
            venue: CANON.to_string(),
            symbol: PERP.to_string(),
            price: 74_300.0,
            timestamp: 7,
        },
    );
    core.dispatch(Ingest::Market);

    let mark_of =
        |index: usize| core.engine(index).price_board.cell(CANON, PERP).and_then(|c| c.mark);
    assert_eq!(
        mark_of(1),
        Some((74_300.0, 7)),
        "the twin's board mark slot, at the mark's own time"
    );
    assert_eq!(
        core.engine(1).account.mark_of(CANON, PERP),
        Some(74_300.0),
        "the twin's account mark"
    );
    assert_eq!(mark_of(0), Some((74_300.0, 7)), "the default engine's board holds the same cell");
    assert_eq!(
        core.engine(0).account.mark_of(CANON, PERP),
        Some(74_300.0),
        "the DEFAULT engine's account mark: the slot the published `marks` list is made of"
    );
    assert!(core.dirty, "a mark marks the core for a publish");
    // The mark outranks a quote on the twin's board (rung one): once a quote reaches it the mark still
    // wins, which is why a routed quote could not change the twin's valuation while the mark lane runs.
    let twin = core.engine_mut(1);
    twin.price_board.set_quote(CANON, PERP, 74_290.0, 74_310.0, 8);
    let config = vike_execution::price_board::PriceBoardConfig::default();
    assert!(matches!(
        twin.price_board.resolve(CANON, PERP, true, 9, &config),
        vike_execution::price_board::PriceBoardResolution::Priced {
            price: px,
            source: vike_execution::price_board::PriceSource::Mark,
            ..
        } if px == 74_300.0
    ));
}

// ---- PATH THREE: the price tap's TOP OF BOOK, side-aware (owner, 2026-11-15; decision 0208's amendment) ----

/// Hand the core the price tap's throttled top of book for `(CANON, symbol)`, as `CoreLaneSink` does.
fn top_of_book(core: &mut CoreThread<RecordingClient>, symbol: &str, bid: f64, ask: f64, ts: i64) {
    core.market
        .state
        .lock()
        .unwrap()
        .top_slots
        .insert((CANON.into(), symbol.into()), (bid, ask, ts));
    core.dispatch(Ingest::Market);
}

/// A bare fill of `qty` (signed) at `price` into engine `index` on `symbol`, with no mark on it.
fn fill_into(
    core: &mut CoreThread<RecordingClient>,
    index: usize,
    symbol: &str,
    qty: f64,
    price: f64,
) {
    let event = Event::Fill(FillEvent {
        trade_id: TradeId::prefixed("fill-", index),
        order_client_id: String::new(),
        venue: CANON.into(),
        symbol: symbol.into(),
        side: if qty > 0.0 { 1 } else { -1 },
        last_qty: qty.abs(),
        last_price: price,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    });
    core.publish_to(index, event);
}

/// **The twin is valued at the price it can CLOSE at: a long at the BID, a short at the ASK, never the mid.**
/// The owner's example (2026-11-15): bid 100 000, ask 100 010, long 1 BTC from 99 900 shows +100, not the
/// mid's +105. The top lands in the twin's QUOTE slots (no mark slot), its equity, own PnL and margin price
/// read the side, and the default engine's account slot (the published `marks`) still says the mid.
#[test]
fn a_top_of_book_values_the_twins_long_at_the_bid_and_a_short_at_the_ask() {
    let mut core = default_and_a_twin();
    fill_into(&mut core, 1, PERP, 1.0, 99_900.0);
    core.dirty = false;
    top_of_book(&mut core, PERP, 100_000.0, 100_010.0, 7);

    let cell = *core.engine(1).price_board.cell(CANON, PERP).expect("the twin's board");
    assert_eq!(
        (cell.bid, cell.ask, cell.mark),
        (Some((100_000.0, 7)), Some((100_010.0, 7)), None),
        "the twin's QUOTE slots, at the top's own time; no mark"
    );
    let config = vike_execution::price_board::PriceBoardConfig::default();
    let twin = core.engine(1);
    assert_eq!(
        twin.resolve_equity(core.seed_of(1), &config).unrealized_total,
        100.0,
        "+100, not +105"
    );
    assert_eq!(twin.resolved_own_pnl(&config), 100.0, "the drawdown latch's term");
    assert_eq!(
        twin.resolved_position_price(CANON, PERP, 1.0, &config),
        Some(100_000.0),
        "long: bid"
    );
    assert_eq!(
        twin.resolved_position_price(CANON, PERP, -1.0, &config),
        Some(100_010.0),
        "short: ask"
    );
    assert_eq!(
        core.engine(0).account.mark_of(CANON, PERP),
        Some(100_005.0),
        "the default engine's account slot (the published `marks`) keeps the mid, valuing nothing"
    );
    assert!(core.dirty, "a top marks the core for a publish");
}

/// **A labelled account's lane never puts a mid ABOVE a default engine's own bid and ask** (the shared-symbol
/// case, owner 2026-11-15). Two accounts of one venue on ONE symbol: the labelled one's lane is marked by the
/// tap, and its top used to land in the DEFAULT engine's mark slot, which outranks the side quote, so the
/// default engine's long was valued at the mid. Now it lands in the quote slots: a short of the default
/// engine reads the ask.
#[test]
fn a_labelled_accounts_top_never_values_the_default_engine_at_the_mid() {
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ALT, CANON, SYMBOL))],
        CoreConfig::default(),
    );
    assert!(core.multi_account, "precondition: two accounts of one venue");
    fill_into(&mut core, 0, SYMBOL, -1.0, 100_110.0);
    core.dispatch(Ingest::Quote(Box::new(vike_execution::QuoteUpdate {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        quote: vike_marketdata::TickQuote {
            timestamp: 5,
            local_timestamp: 0,
            bid: 100_000.0,
            ask: 100_010.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: Default::default(),
        },
    })));
    top_of_book(&mut core, SYMBOL, 100_000.0, 100_010.0, 6);

    let config = vike_execution::price_board::PriceBoardConfig::default();
    for index in [0, 1] {
        assert_eq!(core.engine(index).price_board.cell(CANON, SYMBOL).and_then(|c| c.mark), None);
    }
    assert_eq!(
        core.engine(0).resolve_equity(core.seed_of(0), &config).unrealized_total,
        100.0,
        "short 1 from 100 110 at the ASK 100 010: +100, not the mid's +105"
    );
}

/// **The twin's MARK-armed sell stop triggers on the BID of the tap's top**, never on the mid (owner,
/// 2026-11-15): bid 99 951 leaves it, bid 99 950 (mid 99 955) fires it.
#[test]
fn a_mark_armed_sell_stop_on_the_twins_symbol_fires_when_the_tops_bid_reaches_it() {
    let mut core = default_and_a_twin();
    let armed = core.arm_conditional_as(
        vike_execution::ConditionalIntent {
            venue: CANON.into(),
            symbol: PERP.into(),
            side: -1,
            qty: 1.0,
            price: Some(99_950.0),
            trail: None,
            trigger_by: Some(vike_model::TriggerBy::Mark),
            kind_terms: None,
            account: None,
        },
        false,
        0,
        EngineRoute::Engine(1),
    );
    assert!(armed.is_some(), "precondition: armed on the mark lane");
    let resting = |core: &CoreThread<RecordingClient>| {
        core.emulated.order_emulation_arms.values().map(|b| b.len()).sum::<usize>()
    };
    top_of_book(&mut core, PERP, 99_951.0, 99_959.0, 1);
    assert_eq!(resting(&core), 1, "the bid is above the stop");
    top_of_book(&mut core, PERP, 99_950.0, 99_960.0, 2);
    assert_eq!(resting(&core), 0, "the BID reached 99 950 while the mid is 99 955: fired");
}

/// One account per venue — binance's default at engine 0 on [`SYMBOL`], bybit's only (unlabelled)
/// account at engine 1 on [`SYMBOL_B`] — the shape `vike_tradehub_core::assemble_runtime` builds, and
/// (asserted here) NOT a multi-account process.
fn one_account_per_venue() -> CoreThread<RecordingClient> {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B))],
        CoreConfig::default(),
    );
    assert!(!core.multi_account, "two venues, one account each — not a multi-account process");
    core
}

/// Two accounts of one exchange on ONE symbol, both `Account`s labelled with the CANONICAL venue: the
/// ordinary spread the deleted collision rule refused, and the configuration in which the symbol
/// answers nothing. See [`engine_with_account_venue`] for why that seeding is right for the reconcile
/// path and wrong for the fill-routing path.
fn two_accounts_on_one_symbol() -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
        CoreConfig::default(),
    )
}

#[path = "route_key/gate.rs"]
#[cfg(test)]
mod gate;
#[path = "route_key/payload_capability.rs"]
#[cfg(test)]
mod payload_capability;
#[path = "route_key/quote_mark_slot.rs"]
#[cfg(test)]
mod quote_mark_slot;
#[path = "route_key/symbol_less_payloads.rs"]
#[cfg(test)]
mod symbol_less_payloads;
