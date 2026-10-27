//! Admission of a symbol QUOTED in a currency other than the account's: `ExecutionEngine::admit_symbol`
//! with `SymbolAdmission::{quote_ccy, quote_rate, rate_as_of_ms}` (`crates/vike-exec/src/account/fx.rs`'s
//! module doc is the contract).
//!
//! A USD account trades USDJPY (JPY-quoted) and EURGBP (GBP-quoted). The rate `r` is account-currency
//! units per ONE unit of the quote currency (1/150 for JPY, never 150), it rides the admission, and the
//! engine installs the EFFECTIVE contract value (the venue's unit times `r`) into the account's grid so
//! that every money number comes out in USD with no other edit. These tests pin what an admission does
//! and, just as hard, what a REFUSED one does not do: every refusal word leaves the engine exactly as it
//! was (the one exception, the jump memory, is pinned on its own).
//!
//! This module also owns the small fixtures (`engine`, `quoted`, `fill_event`, ...) that the other
//! `quote_*` members of the engine group import.

use indexmap::IndexMap;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, DEFAULT_QUOTE_RATE_TTL_MS, EngineSnapshot, EventHandler, ExecutionEngine, Fold,
    MAX_QUOTE_RATE_STEP, MIN_QUOTE_RATE_CONFIRM_GAP_MS, Outbox, QUOTE_RATE_FUTURE_SKEW_MS,
    SymbolAdmission,
};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{ContractValue, ManagedOrder, NotionalKind, OrderRequest, RiskLimits, SymbolGrid};

use crate::support::EngineBuilder;

pub(crate) const VENUE: &str = "oanda";
/// Quoted in the account's own currency (USD).
pub(crate) const MOUNTED: &str = "EURUSD";
pub(crate) const USDJPY: &str = "USDJPY";
pub(crate) const GBPJPY: &str = "GBPJPY";
pub(crate) const EURGBP: &str = "EURGBP";
/// The engine clock of every fixture, in ms. Nothing here reads a wall clock.
pub(crate) const T0: i64 = 1_000_000;

/// A USD account mounting `EURUSD`, its clock at [`T0`].
pub(crate) fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    // TELLS the engine its currency, as the node does at assembly: a symbol quoted in another
    // currency is refused until it is (`account-ccy-unset`).
    e.set_account_ccy("USD").expect("a currency code");
    e.now_ms = T0;
    e
}

pub(crate) fn grid() -> SymbolGrid {
    SymbolGrid { tick_size: Some(0.00001), lot_size: Some(1.0), ..SymbolGrid::default() }
}

/// An admission of `symbol` quoted in `ccy` at `rate`, observed at `as_of_ms`.
pub(crate) fn quoted(symbol: &str, ccy: &str, rate: f64, as_of_ms: i64) -> SymbolAdmission {
    SymbolAdmission {
        route_key: VENUE.into(),
        symbol: symbol.into(),
        grid: grid(),
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some(ccy.into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
    }
}

/// An admission of a symbol quoted in the account's own currency: no currency, no rate.
pub(crate) fn plain(symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        ..quoted(symbol, "", 0.0, 0)
    }
}

/// An engine holding USDJPY at `rate` (observed at [`T0`]).
pub(crate) fn usdjpy_engine(rate: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", rate, T0)), AdmitOutcome::Admitted);
    e
}

pub(crate) fn fill_event(
    trade_id: &'static str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: T0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// Fold one fill through the engine's event door.
pub(crate) fn fold(e: &mut ExecutionEngine<RecordingClient>, f: FillEvent) -> Fold {
    e.on_event(&Event::Fill(f), &mut Outbox::default())
}

pub(crate) fn size(e: &ExecutionEngine<RecordingClient>, symbol: &str) -> Option<f64> {
    e.account.positions.get(&(VENUE.into(), symbol.into(), PositionSide::Both)).map(|p| p.size)
}

/// Everything an admission may write, in comparable form.
type Fingerprint = (Vec<String>, RiskLimits, IndexMap<String, ContractValue>, EngineSnapshot);

fn fingerprint(e: &ExecutionEngine<RecordingClient>) -> Fingerprint {
    (
        e.extra_symbols.clone(),
        e.gate.limits.clone(),
        (*e.account.contract_value_grid()).clone(),
        e.snapshot_state(),
    )
}

/// `a` is refused with `word` and the engine is exactly as it was.
fn assert_refused(
    e: &mut ExecutionEngine<RecordingClient>,
    a: &SymbolAdmission,
    word: &'static str,
) {
    let before = fingerprint(e);
    assert_eq!(e.admit_symbol(a), AdmitOutcome::Refused(word), "{a:?}");
    assert!(fingerprint(e) == before, "a refusal ({word}) changed the engine: {a:?}");
}

/// USDJPY (JPY, 1/150) and a plain GBPUSD, both held.
fn held() -> ExecutionEngine<RecordingClient> {
    let mut e = usdjpy_engine(1.0 / 150.0);
    assert_eq!(e.admit_symbol(&plain("GBPUSD")), AdmitOutcome::Admitted);
    e
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

#[test]
fn a_foreign_admission_installs_the_venues_unit_times_the_rate() {
    let mut e = engine();
    let r = 1.0 / 150.0;
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", r, T0)), AdmitOutcome::Admitted);

    let v = e.account.contract_value_of(USDJPY);
    assert_eq!(v.kind, NotionalKind::Linear);
    assert_eq!(bits(v.per_unit), bits(r), "the unit 1.0 times r is r, bit for bit");
    assert!(e.account.contract_value_grid().contains_key(USDJPY), "listed even when it is par");

    assert!(e.accepts_symbol(USDJPY));
    assert!(e.account.has_foreign_quote());
    assert!(e.account.is_foreign_quoted(USDJPY));
    assert_eq!(e.account.quote_ccy_of(USDJPY), Some("JPY"));
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(r));
    assert_eq!(e.account.quote_leg(USDJPY, T0), (r, false));
    assert!(e.gate.limits.grid_by_symbol[USDJPY].on_demand, "an ordinary on-demand grid row");

    // an account-quoted symbol and the mounted one are not touched by it
    assert_eq!(e.account.contract_value_of(MOUNTED), ContractValue::linear(1.0));
    assert_eq!(e.account.quote_rate_of(MOUNTED), 1.0);
    assert_eq!(e.account.quote_rate_of("NEVER-HEARD-OF"), 1.0);
    assert_eq!(e.account.quote_leg(MOUNTED, T0 + 10 * DEFAULT_QUOTE_RATE_TTL_MS), (1.0, false));
    assert_eq!(e.account.quote_ccy_of(MOUNTED), None);
}

#[test]
fn a_contract_value_in_the_admission_is_the_venues_unit_scaled_by_the_rate() {
    let mut e = engine();
    let r = 1.0 / 150.0;
    let lot = SymbolAdmission {
        contract_value: Some(ContractValue::linear(1000.0)),
        ..quoted(USDJPY, "JPY", r, T0)
    };
    assert_eq!(e.admit_symbol(&lot), AdmitOutcome::Admitted);
    let v = e.account.contract_value_of(USDJPY);
    assert_eq!(v.kind, NotionalKind::Linear);
    assert_eq!(bits(v.per_unit), bits(1000.0 * r));

    let inverse = SymbolAdmission {
        contract_value: Some(ContractValue::inverse(100.0)),
        ..quoted(GBPJPY, "JPY", r, T0)
    };
    assert_eq!(e.admit_symbol(&inverse), AdmitOutcome::Admitted);
    let v = e.account.contract_value_of(GBPJPY);
    assert_eq!(v.kind, NotionalKind::Inverse, "the kind survives the conversion");
    assert_eq!(bits(v.per_unit), bits(100.0 * r));
}

/// THE refusal table. Each row is a distinct way an admission cannot be taken as given, and each one
/// is answered with its own word and NOTHING written: not the symbol set, not the limits, not the
/// contract-value grid, not one byte of the snapshot.
#[test]
fn every_refusal_word_is_named_and_changes_nothing() {
    let r = 1.0 / 150.0;
    let ttl = DEFAULT_QUOTE_RATE_TTL_MS;
    let new = |rate: Option<f64>, as_of: Option<i64>| SymbolAdmission {
        quote_rate: rate,
        rate_as_of_ms: as_of,
        ..quoted("AUDJPY", "JPY", r, T0)
    };
    let mut e = held();

    // no-quote-rate: no rate, a rate without its instant, an instant without its rate, and any rate
    // that is not a finite number in the range the engine believes at all
    assert_refused(&mut e, &new(None, None), "no-quote-rate");
    assert_refused(&mut e, &new(Some(r), None), "no-quote-rate");
    assert_refused(&mut e, &new(None, Some(T0)), "no-quote-rate");
    for bad in [f64::NAN, 0.0, -0.0, -r, f64::INFINITY, f64::NEG_INFINITY, 1e-13, 1e13] {
        assert_refused(&mut e, &new(Some(bad), Some(T0)), "no-quote-rate");
    }

    // quote-rate-stale: older than the window on ARRIVAL (the boundary itself is pinned below)
    assert_refused(&mut e, &new(Some(r), Some(T0 - ttl - 1)), "quote-rate-stale");

    // quote-rate-future: stamped ahead of the engine's clock by more than the skew (`quote_review.rs`
    // pins the boundary and the stamps that matter: µs, ns, `i64::MAX`)
    let skew = QUOTE_RATE_FUTURE_SKEW_MS;
    assert_refused(&mut e, &new(Some(r), Some(T0 + skew + 1)), "quote-rate-future");

    // quote-ccy-missing: a rate with no currency, a blank currency, and a held foreign symbol that
    // comes back without the currency it was admitted under
    let no_ccy = SymbolAdmission { quote_ccy: None, ..quoted("AUDJPY", "JPY", r, T0) };
    assert_refused(&mut e, &no_ccy, "quote-ccy-missing");
    let only_instant = SymbolAdmission { quote_rate: None, ..no_ccy.clone() };
    assert_refused(&mut e, &only_instant, "quote-ccy-missing");
    for blank in ["", "  "] {
        assert_refused(&mut e, &quoted("AUDJPY", blank, r, T0), "quote-ccy-missing");
    }
    assert_refused(&mut e, &plain(USDJPY), "quote-ccy-missing");
    assert_refused(
        &mut e,
        &SymbolAdmission { quote_ccy: None, ..quoted(USDJPY, "JPY", r, T0 + 1) },
        "quote-ccy-missing",
    );

    // quote-ccy-changed: held under JPY, now claimed under another currency or under the account's own
    assert_refused(&mut e, &quoted(USDJPY, "GBP", 1.35, T0), "quote-ccy-changed");
    assert_refused(&mut e, &quoted(USDJPY, "USD", 1.0, T0), "quote-ccy-changed");

    // quote-rate-range: a valid rate whose product with the unit leaves the representable range
    let huge = SymbolAdmission {
        contract_value: Some(ContractValue::linear(1e300)),
        ..quoted("AUDJPY", "JPY", 1e10, T0)
    };
    // (a new currency, so no standing rate judges the step)
    assert_refused(
        &mut e,
        &SymbolAdmission { quote_ccy: Some("CHF".into()), ..huge },
        "quote-rate-range",
    );

    // and none of it admitted anything
    assert!(!e.accepts_symbol("AUDJPY"));
    assert_eq!(e.account.quote_ccy_of(USDJPY), Some("JPY"));
}

/// Refusals on a symbol that is NEW and an engine that holds no foreign symbol yet leave no fx book
/// behind either (the book is created by the first symbol that is taken).
#[test]
fn a_refused_first_foreign_admission_leaves_no_fx_book() {
    let mut e = engine();
    let before = fingerprint(&e);
    for a in [
        quoted(USDJPY, "JPY", f64::NAN, T0),
        quoted(USDJPY, "JPY", 1.0 / 150.0, T0 - DEFAULT_QUOTE_RATE_TTL_MS - 1),
        SymbolAdmission { quote_ccy: None, ..quoted(USDJPY, "JPY", 1.0 / 150.0, T0) },
    ] {
        assert!(matches!(e.admit_symbol(&a), AdmitOutcome::Refused(_)), "{a:?}");
    }
    assert!(fingerprint(&e) == before);
    assert!(!e.account.has_foreign_quote());
    assert!(e.snapshot_state().account.fx.is_none());
}

#[test]
fn the_stale_window_is_judged_at_admission_with_the_engine_clock() {
    let r = 1.0 / 150.0;
    let mut e = engine();
    // exactly the window old: taken (the engine clock is T0)
    let edge = quoted(USDJPY, "JPY", r, T0 - DEFAULT_QUOTE_RATE_TTL_MS);
    assert_eq!(e.admit_symbol(&edge), AdmitOutcome::Admitted);
    // one ms older: refused
    let mut f = engine();
    let past = quoted(USDJPY, "JPY", r, T0 - DEFAULT_QUOTE_RATE_TTL_MS - 1);
    assert_eq!(f.admit_symbol(&past), AdmitOutcome::Refused("quote-rate-stale"));
}

/// The step cap is inclusive: a rate exactly `MAX_QUOTE_RATE_STEP` away (either way) is a move, one
/// beyond it is a jump.
#[test]
fn the_step_cap_is_inclusive_in_both_directions() {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted("USDCHF", "CHF", 2.0, T0)), AdmitOutcome::Admitted);
    let up = 2.0 * MAX_QUOTE_RATE_STEP;
    assert_eq!(up, 3.0, "the fixture is exact in binary");
    assert_eq!(e.admit_symbol(&quoted("USDCHF", "CHF", up, T0 + 1)), AdmitOutcome::Refreshed);
    assert_eq!(bits(e.account.quote_rate_of("USDCHF")), bits(3.0));
    // down by exactly the cap: 3.0 / 2.0 == 1.5
    assert_eq!(e.admit_symbol(&quoted("USDCHF", "CHF", 2.0, T0 + 2)), AdmitOutcome::Refreshed);
    assert_eq!(bits(e.account.quote_rate_of("USDCHF")), bits(2.0));
    // beyond the cap both ways
    let beyond = f64::from_bits(up.to_bits() + 1);
    assert_eq!(
        e.admit_symbol(&quoted("USDCHF", "CHF", beyond, T0 + 3)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    assert_eq!(
        e.admit_symbol(&quoted("USDCHF", "CHF", 2.0 / 1.51, T0 + 4)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    assert_eq!(bits(e.account.quote_rate_of("USDCHF")), bits(2.0), "the standing rate stands");
}

/// A jump is refused and remembered, and the remembering is ALL it changes: the rate that stands, the
/// effective value, the symbols, the limits are as they were.
#[test]
fn a_jump_changes_nothing_but_the_memory_of_it() {
    let mut e = held();
    let before = e.snapshot_state();
    let grid_before = (*e.account.contract_value_grid()).clone();
    let limits_before = e.gate.limits.clone();
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", 150.0, T0 + 1)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    assert_eq!(*e.account.contract_value_grid(), grid_before);
    assert_eq!(e.gate.limits, limits_before);
    let mut after = e.snapshot_state();
    let rows = &mut after.account.fx.as_mut().expect("fx").rates;
    assert_eq!(rows[0].pending, Some(150.0), "the refused rate is what was remembered");
    assert_eq!(rows[0].pending_as_of_ms, Some(T0 + 1), "...with the stamp of the offer");
    rows[0].pending = None;
    rows[0].pending_as_of_ms = None;
    assert_eq!(after, before, "nothing but the memory moved");
}

/// The flipped pair (150 where 1/150 stands) is a 22,500x step. Refused; and no sequence of good rates
/// and flipped ones makes the flipped one stick, because a rate the engine takes clears the memory.
#[test]
fn a_lone_flipped_rate_is_refused_and_never_accepted_by_itself() {
    let r = 1.0 / 150.0;
    let mut e = usdjpy_engine(r);
    let stands = |e: &ExecutionEngine<RecordingClient>| {
        assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(r));
        assert_eq!(bits(e.account.contract_value_of(USDJPY).per_unit), bits(r));
    };
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", 150.0, T0 + 1)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    stands(&e);
    // a good rate arrives (the pair is quoted again the right way up): taken, memory cleared
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", r, T0 + 2)), AdmitOutcome::Refreshed);
    // the flipped one again is a FIRST offer again: refused, not "sent twice"
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", 150.0, T0 + 3)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    stands(&e);
    // the same flipped rate within the 0.1% resend window, from a LATER observation (a second after the
    // refused one): the engine takes it as a real move. That is the documented price of letting a
    // genuine devaluation through; it takes TWO identical offers, from two observations
    let later = T0 + 3 + MIN_QUOTE_RATE_CONFIRM_GAP_MS;
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", 150.0, later)), AdmitOutcome::Refreshed);
}

#[test]
fn a_refused_jump_sent_again_is_taken_as_a_real_move() {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", 0.01, T0)), AdmitOutcome::Admitted);
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", 0.02, T0 + 1)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(0.01), "refused: the old rate stands");
    // the same rate, 0.05% off the remembered one: inside the 0.1% resend window
    let again = T0 + 1 + MIN_QUOTE_RATE_CONFIRM_GAP_MS;
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", 0.02001, again)), AdmitOutcome::Refreshed);
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(0.02001));
    assert_eq!(bits(e.account.contract_value_of(USDJPY).per_unit), bits(0.02001));
    let row = e.snapshot_state().account.fx.expect("fx").rates.remove(0);
    assert!(
        row.pending.is_none() && row.pending_as_of_ms.is_none(),
        "taken: nothing left to confirm"
    );
}

/// Only the LATEST refused rate is remembered: a different jump replaces the memory, so confirming the
/// first one is a first offer again. (Each offer is a second after the last: a later observation each
/// time, so each one replaces the memory it did not confirm.)
#[test]
fn the_memory_holds_only_the_latest_refused_rate() {
    let mut e = engine();
    e.admit_symbol(&quoted(USDJPY, "JPY", 0.01, T0));
    let offer = |e: &mut ExecutionEngine<RecordingClient>, rate: f64, second: i64| {
        e.admit_symbol(&quoted(USDJPY, "JPY", rate, T0 + second * MIN_QUOTE_RATE_CONFIRM_GAP_MS))
    };
    assert_eq!(offer(&mut e, 0.02, 1), AdmitOutcome::Refused("quote-rate-jump"));
    assert_eq!(offer(&mut e, 0.03, 2), AdmitOutcome::Refused("quote-rate-jump"));
    assert_eq!(offer(&mut e, 0.02, 3), AdmitOutcome::Refused("quote-rate-jump"), "memory is 0.03");
    assert_eq!(offer(&mut e, 0.02, 4), AdmitOutcome::Refreshed, "now it was sent twice in a row");
}

/// One re-admission is the whole refresh: EVERY symbol quoted in that currency is rewritten from its
/// own base and the new rate, and a symbol quoted in another currency is not touched.
#[test]
fn a_refresh_rewrites_every_symbol_of_the_currency_and_leaves_another_alone() {
    let (r1, r2) = (1.0 / 150.0, 1.0 / 160.0);
    let mut e = engine();
    e.admit_symbol(&quoted(USDJPY, "JPY", r1, T0));
    let gbpjpy = SymbolAdmission {
        contract_value: Some(ContractValue::linear(10.0)),
        ..quoted(GBPJPY, "JPY", r1, T0)
    };
    e.admit_symbol(&gbpjpy);
    e.admit_symbol(&quoted(EURGBP, "GBP", 1.35, T0));
    let gbp_before = e.account.contract_value_of(EURGBP);

    // the node re-admits USDJPY alone, with a newer rate
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", r2, T0 + 5_000)), AdmitOutcome::Refreshed);

    assert_eq!(bits(e.account.contract_value_of(USDJPY).per_unit), bits(r2));
    let want = ContractValue::linear(10.0).in_account(r2).expect("representable");
    assert_eq!(
        e.account.contract_value_of(GBPJPY),
        want,
        "GBPJPY follows from its OWN base (10) and the new rate, not from the old effective value"
    );
    assert_eq!(bits(e.account.quote_rate_of(GBPJPY)), bits(r2));
    assert_eq!(e.account.contract_value_of(EURGBP), gbp_before, "GBP is another currency");
    assert_eq!(bits(e.account.quote_rate_of(EURGBP)), bits(1.35));

    // the published table: one row per currency, first-use order, the rate and its instant
    let (account_ccy, rows) = e.account.quote_rate_table(T0 + 5_000);
    assert_eq!(account_ccy, "USD");
    let table: Vec<(&str, u64, i64, bool)> =
        rows.iter().map(|r| (r.ccy.as_str(), bits(r.rate), r.as_of_ms, r.stale)).collect();
    assert_eq!(table, vec![("JPY", bits(r2), T0 + 5_000, false), ("GBP", bits(1.35), T0, false)]);
}

/// The table's age verdict is the gate's: fresh through the window, stale one ms after it.
#[test]
fn the_published_table_flags_a_rate_stale_exactly_when_the_gate_does() {
    let e = usdjpy_engine(1.0 / 150.0);
    let stale_at = |now: i64| e.account.quote_rate_table(now).1[0].stale;
    assert!(!stale_at(T0 + DEFAULT_QUOTE_RATE_TTL_MS));
    assert!(stale_at(T0 + DEFAULT_QUOTE_RATE_TTL_MS + 1));
    assert_eq!(e.account.quote_rate_table(T0).0, "USD");
    let none = engine();
    let (ccy, rows) = none.account.quote_rate_table(T0);
    assert!(ccy.is_empty() && rows.is_empty(), "no foreign symbol, no table");
}

/// A refresh that would push ANOTHER symbol of the currency out of the representable range is refused
/// whole: the new symbol is not written either.
#[test]
fn a_refresh_that_pushes_a_sibling_out_of_range_is_refused_and_nothing_moves() {
    let mut e = engine();
    // a parity currency (rate exactly 1.0), and a symbol whose unit is near the top of the range
    let big = SymbolAdmission {
        contract_value: Some(ContractValue::linear(1.5e308)),
        ..quoted("BIGPAR", "PAR", 1.0, T0)
    };
    assert_eq!(e.admit_symbol(&big), AdmitOutcome::Admitted);
    let before = fingerprint(&e);
    // 1.4 is inside the step cap, so only the range check can refuse it: 1.5e308 * 1.4 overflows
    let other = quoted("OTHERPAR", "PAR", 1.4, T0 + 1);
    assert_eq!(e.admit_symbol(&other), AdmitOutcome::Refused("quote-rate-range"));
    assert!(fingerprint(&e) == before);
    assert!(!e.accepts_symbol("OTHERPAR"));
}

/// An rate OLDER than the standing one is not taken; the symbol is admitted at the standing one.
#[test]
fn an_older_rate_is_not_taken_and_the_symbol_is_admitted_at_the_standing_one() {
    let r = 1.0 / 150.0;
    let mut e = engine();
    e.admit_symbol(&quoted(USDJPY, "JPY", r, T0 + 10));

    // a refresh with an older instant (but inside the window) keeps the standing rate
    let older = quoted(USDJPY, "JPY", 1.0 / 155.0, T0 + 5);
    assert_eq!(e.admit_symbol(&older), AdmitOutcome::Refreshed);
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(r));
    assert_eq!(e.account.quote_rate_table(T0 + 10).1[0].as_of_ms, T0 + 10);

    // ...and a NEW symbol quoted in the same currency joins at the standing rate
    let newcomer = quoted(GBPJPY, "JPY", 1.0 / 155.0, T0 + 5);
    assert_eq!(e.admit_symbol(&newcomer), AdmitOutcome::Admitted);
    assert_eq!(bits(e.account.contract_value_of(GBPJPY).per_unit), bits(r));
    assert_eq!(bits(e.account.contract_value_of(USDJPY).per_unit), bits(r));
}

/// A refresh that states no contract value keeps the base an earlier admission installed.
#[test]
fn a_refresh_without_a_contract_value_keeps_the_base() {
    let (r1, r2) = (1.0 / 150.0, 1.0 / 152.0);
    let mut e = engine();
    let first = SymbolAdmission {
        contract_value: Some(ContractValue::linear(10.0)),
        ..quoted(USDJPY, "JPY", r1, T0)
    };
    e.admit_symbol(&first);
    assert_eq!(bits(e.account.contract_value_of(USDJPY).per_unit), bits(10.0 * r1));
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", r2, T0 + 1)), AdmitOutcome::Refreshed);
    assert_eq!(
        bits(e.account.contract_value_of(USDJPY).per_unit),
        bits(10.0 * r2),
        "the base (10) times the new rate; neither the default 1.0 nor the old effective value"
    );
}

/// A refresh that DOES state one replaces the base.
#[test]
fn a_refresh_with_a_contract_value_replaces_the_base() {
    let r = 1.0 / 150.0;
    let mut e = engine();
    let ten = SymbolAdmission {
        contract_value: Some(ContractValue::linear(10.0)),
        ..quoted(USDJPY, "JPY", r, T0)
    };
    e.admit_symbol(&ten);
    let hundred =
        SymbolAdmission { contract_value: Some(ContractValue::linear(100.0)), ..ten.clone() };
    assert_eq!(e.admit_symbol(&hundred), AdmitOutcome::Refreshed);
    assert_eq!(bits(e.account.contract_value_of(USDJPY).per_unit), bits(100.0 * r));
}

/// Naming the ACCOUNT'S own currency is an ordinary account-quoted symbol: no fx book, no rate, and the
/// engine is exactly the one an admission that names no currency builds.
#[test]
fn an_admission_in_the_accounts_own_currency_is_an_ordinary_symbol() {
    let mut named = engine();
    let mut unnamed = engine();
    let usd = SymbolAdmission { quote_ccy: Some("USD".into()), ..plain("GBPUSD") };
    assert_eq!(named.admit_symbol(&usd), AdmitOutcome::Admitted);
    assert_eq!(unnamed.admit_symbol(&plain("GBPUSD")), AdmitOutcome::Admitted);
    assert!(!named.account.has_foreign_quote());
    assert!(!named.account.is_foreign_quoted("GBPUSD"));
    assert_eq!(named.account.quote_rate_of("GBPUSD"), 1.0);
    assert!(named.snapshot_state() == unnamed.snapshot_state());

    // a contract value in it goes through the ordinary path, unscaled
    let ten = SymbolAdmission { contract_value: Some(ContractValue::linear(10.0)), ..usd };
    assert_eq!(named.admit_symbol(&ten), AdmitOutcome::Refreshed);
    assert_eq!(named.account.contract_value_of("GBPUSD"), ContractValue::linear(10.0));
    assert!(!named.account.has_foreign_quote());
}

/// FLIPPED by the mounted-symbol amendment of decision 0183: the mounted symbol used to stay `Mounted`
/// and write nothing whatever currency the admission named, because the engine booked it at par in its
/// own quote money. A mount quoted in the LEDGER's own money still reads nothing from an admission
/// (the admission names the ledger's currency, or none): `quote_mount.rs` holds the mount that is not.
#[test]
fn the_mounted_symbol_in_the_ledgers_own_money_stays_mounted_whatever_the_admission_says() {
    let mut e = engine();
    let before = fingerprint(&e);
    for a in [quoted(MOUNTED, "USD", 1.0, T0), plain(MOUNTED), quoted(MOUNTED, " usd ", 1.0, T0)] {
        assert_eq!(e.admit_symbol(&a), AdmitOutcome::Mounted, "{a:?}");
        assert!(fingerprint(&e) == before, "nothing written: {a:?}");
    }
    assert!(!e.account.has_foreign_quote());
    assert!(e.extra_symbols.is_empty());
}

/// A fill the engine does not ACCEPT as a symbol but OWNS by the coid of an order it manages (a combo
/// leg, an option ticket) has a currency the engine does not know once the ledger holds a foreign
/// symbol. Folded at par it would be wrong by the whole rate, so it is dropped; an engine with no
/// foreign symbol folds it as it always did.
#[test]
fn an_owned_fill_on_an_unknown_symbol_is_dropped_once_a_foreign_symbol_is_held() {
    let owned = |e: &mut ExecutionEngine<RecordingClient>| {
        let req = OrderRequest {
            client_order_id: "own1".into(),
            venue: VENUE.into(),
            symbol: "XAUUSD".into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(2000.0),
            ..Default::default()
        };
        e.registry.insert("own1".to_string(), ManagedOrder::new(req));
    };
    let ticket = || FillEvent {
        client_order_id: "own1".into(),
        ..fill_event("t1", "XAUUSD", 1, 1.0, 2000.0)
    };

    let mut with_fx = usdjpy_engine(1.0 / 150.0);
    owned(&mut with_fx);
    assert!(!with_fx.accepts_symbol("XAUUSD"));
    assert!(matches!(fold(&mut with_fx, ticket()), Fold::Dropped));
    assert_eq!(size(&with_fx, "XAUUSD"), None, "nothing folded");
    assert!(!with_fx.account.has_folded_fill("t1"), "a dropped fill does not burn its trade id");

    let mut plain_engine = engine();
    owned(&mut plain_engine);
    assert!(matches!(fold(&mut plain_engine, ticket()), Fold::Applied));
    assert_eq!(size(&plain_engine, "XAUUSD"), Some(1.0), "no foreign symbol: folded as ever");

    // an ACCEPTED foreign symbol folds, of course
    assert!(matches!(
        fold(&mut with_fx, fill_event("t2", USDJPY, 1, 1000.0, 150.0)),
        Fold::Applied
    ));
    assert_eq!(size(&with_fx, USDJPY), Some(1000.0));
}

/// A10. The engine's constants are PINNED BY VALUE: every one of them is a risk number (how old a rate
/// may be before opening is denied, how far ahead of the clock a stamp is believed, how long apart two
/// offers must be to confirm a refused jump, how far a rate may move in one step), and a test that only
/// names them would pass whatever they were changed to.
#[test]
fn the_quote_rate_constants_are_pinned_by_value() {
    assert_eq!(DEFAULT_QUOTE_RATE_TTL_MS, 120_000);
    assert_eq!(QUOTE_RATE_FUTURE_SKEW_MS, 5_000);
    assert_eq!(MIN_QUOTE_RATE_CONFIRM_GAP_MS, 1_000);
    assert_eq!(MAX_QUOTE_RATE_STEP, 1.5);
}

/// A11. The bounds of a usable rate, both ends and just outside them: a rate in `[1e-12, 1e12]` is
/// admitted (a first rate has nothing to be compared with, so no jump cap applies), one a hair outside is
/// refused `no-quote-rate`. AUDJPY is a real pair at a real scale; the bounds exist so that
/// `unit x rate` stays representable, not as a market sanity check.
#[test]
fn a_rate_is_usable_between_1e_minus_12_and_1e_12_inclusive_and_not_a_hair_outside() {
    for (rate, admitted) in
        [(1e-12, true), (1e12, true), (1e-12 * 0.99, false), (1e12 * 1.01, false)]
    {
        let mut e = engine();
        let outcome = e.admit_symbol(&quoted("AUDJPY", "JPY", rate, T0));
        if admitted {
            assert_eq!(outcome, AdmitOutcome::Admitted, "{rate:e} is inside the bounds");
            assert_eq!(e.account.quote_rate_of("AUDJPY").to_bits(), rate.to_bits());
        } else {
            assert_eq!(outcome, AdmitOutcome::Refused("no-quote-rate"), "{rate:e} is outside");
            assert!(!e.account.has_foreign_quote(), "and nothing was admitted");
        }
    }
}
