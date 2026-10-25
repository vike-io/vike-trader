//! The multi-currency book through the journal `Snap` round trip (`snapshot_state`, `from_snapshot`,
//! `restore_runtime_state`) and `state_hash`.
//!
//! The snapshot writes the BASE contract value of every symbol (`mult` / `inverse`, as ever) and the
//! rate of each quote currency in a sparse `account.fx` table, never the product of the two, so a
//! restore recomputes the effective value with the function the admission used and a rate that moved
//! after the snapshot leaves no divide-back rounding behind. An engine with no foreign-quoted symbol
//! writes no `fx` key at all, which is what keeps every recorded `state_hash` valid
//! (`crates/vike-exec/src/engine_snapshot.rs`'s `state_hash_of_a_default_limits_snapshot_is_pinned`
//! pins the default bytes; `quote_identity.rs` pins a whole scenario against `origin/main`).

use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, DEFAULT_QUOTE_RATE_TTL_MS, EngineSnapshot, ExecutionEngine, Fold,
    MIN_QUOTE_RATE_CONFIRM_GAP_MS, Outbox, SymbolAdmission, state_hash,
};
use vike_model::events::Event;
use vike_model::{ContractValue, NotionalKind, OrderRequest, OrderType};

use crate::quote_currency::{
    EURGBP, GBPJPY, MOUNTED, T0, USDJPY, VENUE, engine, fill_event, fold, plain, quoted, size,
    usdjpy_engine,
};

const Q: f64 = 100_000.0;
const R: f64 = 1.0 / 150.0;

fn bits(x: f64) -> u64 {
    x.to_bits()
}

/// USD account: USDJPY and GBPJPY (a 10x unit) in JPY, EURGBP in GBP, a plain GBPUSD; a long position
/// in USDJPY and one in GBPUSD.
fn foreign_engine() -> ExecutionEngine<RecordingClient> {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", R, T0)), AdmitOutcome::Admitted);
    let gbpjpy = SymbolAdmission {
        contract_value: Some(ContractValue::linear(10.0)),
        ..quoted(GBPJPY, "JPY", R, T0)
    };
    assert_eq!(e.admit_symbol(&gbpjpy), AdmitOutcome::Admitted);
    assert_eq!(e.admit_symbol(&quoted(EURGBP, "GBP", 1.35, T0)), AdmitOutcome::Admitted);
    assert_eq!(e.admit_symbol(&plain("GBPUSD")), AdmitOutcome::Admitted);
    fold(&mut e, fill_event("d1", USDJPY, 1, Q, 150.0));
    fold(&mut e, fill_event("d2", "GBPUSD", 1, 1000.0, 1.25));
    e
}

fn limit(coid: &str, symbol: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: OrderType::Limit,
        price: Some(150.0),
        reduce_only,
        ..Default::default()
    }
}

/// Submit at `now_ms`; the reason it was denied, or `None` when it reached the venue client.
fn submit(
    e: &mut ExecutionEngine<RecordingClient>,
    req: &OrderRequest,
    now_ms: i64,
) -> Option<String> {
    let mut ob = Outbox::default();
    e.submit_order(req, now_ms, &mut ob);
    ob.0.into_iter().find_map(|ev| match ev {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

fn priced(e: &mut ExecutionEngine<RecordingClient>, symbol: &str, px: f64, at: i64) {
    e.price_board.set_mark(VENUE, symbol, px, at);
}

#[test]
fn a_foreign_snapshot_carries_the_fx_table_and_the_base_values() {
    let snap = foreign_engine().snapshot_state();
    let fx = snap.account.fx.as_ref().expect("an engine with foreign symbols writes an fx table");
    assert_eq!(fx.account_ccy, "USD");
    let want_symbols = [(USDJPY, "JPY"), (GBPJPY, "JPY"), (EURGBP, "GBP")];
    assert_eq!(
        fx.symbols,
        want_symbols.map(|(s, c)| (s.to_string(), c.to_string())).to_vec(),
        "admission order; the account-quoted GBPUSD is not in it"
    );
    let rates: Vec<(&str, u64, i64, Option<f64>)> =
        fx.rates.iter().map(|r| (r.ccy.as_str(), bits(r.rate), r.as_of_ms, r.pending)).collect();
    assert_eq!(rates, vec![("JPY", bits(R), T0, None), ("GBP", bits(1.35), T0, None)]);

    // `mult` keeps the BASE value: the venue's own unit, not its product with the rate
    let mult: Vec<(&str, u64)> =
        snap.account.mult.iter().map(|(s, v)| (s.as_str(), bits(*v))).collect();
    assert_eq!(mult, vec![(USDJPY, bits(1.0)), (GBPJPY, bits(10.0)), (EURGBP, bits(1.0))]);
    assert!(snap.account.inverse.is_empty());

    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(
        json.contains(r#""fx":{"account_ccy":"USD","symbols":[["USDJPY","JPY"],["GBPJPY","JPY"],["EURGBP","GBP"]],"rates":[{"ccy":"JPY","rate":"#),
        "{json}"
    );
    assert!(!json.contains("pending"), "no jump to remember, no key");
}

/// An INVERSE base is written to the inverse list as its base, and comes back an inverse effective value.
#[test]
fn an_inverse_base_is_written_as_its_base_and_restored_as_an_inverse_effective_value() {
    let mut e = engine();
    let xau = SymbolAdmission {
        contract_value: Some(ContractValue::inverse(100.0)),
        ..quoted("XAUJPY", "JPY", R, T0)
    };
    assert_eq!(e.admit_symbol(&xau), AdmitOutcome::Admitted);
    let snap = e.snapshot_state();
    assert!(snap.account.mult.is_empty());
    assert_eq!(snap.account.inverse.len(), 1);
    assert_eq!(snap.account.inverse[0].0, "XAUJPY");
    assert_eq!(bits(snap.account.inverse[0].1), bits(100.0), "the base, not 100 / 150");

    let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    let v = back.account.contract_value_of("XAUJPY");
    assert_eq!(v.kind, NotionalKind::Inverse);
    assert_eq!(bits(v.per_unit), bits(100.0 * R));
    assert_eq!(back.snapshot_state(), snap);
}

#[test]
fn from_snapshot_reproduces_the_engine_exactly_and_the_effective_grid_to_the_bit() {
    let e = foreign_engine();
    let snap = e.snapshot_state();
    let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());

    assert_eq!(back.snapshot_state(), snap);
    assert_eq!(state_hash(&[back.snapshot_state()]), state_hash(std::slice::from_ref(&snap)));

    let (live, restored) = (e.account.contract_value_grid(), back.account.contract_value_grid());
    assert_eq!(live.len(), restored.len());
    for (symbol, v) in live.iter() {
        let b =
            restored.get(symbol).unwrap_or_else(|| panic!("{symbol} is missing after the restore"));
        assert_eq!(b.kind, v.kind, "{symbol}");
        assert_eq!(bits(b.per_unit), bits(v.per_unit), "{symbol}: the effective value, to the bit");
    }
    for symbol in [USDJPY, GBPJPY, EURGBP, "GBPUSD", MOUNTED] {
        assert_eq!(back.account.quote_leg(symbol, T0), e.account.quote_leg(symbol, T0), "{symbol}");
        assert_eq!(back.account.quote_ccy_of(symbol), e.account.quote_ccy_of(symbol), "{symbol}");
        assert_eq!(back.accepts_symbol(symbol), e.accepts_symbol(symbol), "{symbol}");
    }
    assert_eq!(back.account.quote_rate_table(T0), e.account.quote_rate_table(T0));
    assert_eq!(size(&back, USDJPY), Some(Q));

    // through the JSON the journal writes: the same bits (serde_json round-trips floats exactly)
    let json = serde_json::to_string(&snap).expect("serializes");
    let parsed: EngineSnapshot = serde_json::from_str(&json).expect("parses");
    assert_eq!(parsed, snap);
    let from_json = ExecutionEngine::from_snapshot(&parsed, RecordingClient::default());
    assert_eq!(
        bits(from_json.account.contract_value_of(GBPJPY).per_unit),
        bits(ContractValue::linear(10.0).in_account(R).expect("representable").per_unit)
    );
}

/// Snapshot at r1, refresh to r2 afterwards, restore the OLD snapshot, apply r2: the restored engine
/// ends where the live one did. A snapshot is the book as it was; it holds no memory of the later rate.
#[test]
fn a_restore_across_a_rate_change_ends_where_the_live_engine_did() {
    let r2 = 1.0 / 155.0;
    let mut live = foreign_engine();
    let snap1 = live.snapshot_state();
    live.now_ms = T0 + 30_000;
    assert_eq!(live.admit_symbol(&quoted(USDJPY, "JPY", r2, T0 + 30_000)), AdmitOutcome::Refreshed);
    let snap2 = live.snapshot_state();
    assert_ne!(
        state_hash(std::slice::from_ref(&snap1)),
        state_hash(std::slice::from_ref(&snap2)),
        "the rate is hashed"
    );

    // the old snapshot restores the OLD rate for every JPY symbol, from the bases
    let mut back = ExecutionEngine::from_snapshot(&snap1, RecordingClient::default());
    assert_eq!(bits(back.account.contract_value_of(USDJPY).per_unit), bits(R));
    assert_eq!(bits(back.account.contract_value_of(GBPJPY).per_unit), bits(10.0 * R));

    // ...and the node's re-admission brings the new one: the whole currency follows
    back.now_ms = T0 + 30_000;
    assert_eq!(back.admit_symbol(&quoted(USDJPY, "JPY", r2, T0 + 30_000)), AdmitOutcome::Refreshed);
    for symbol in [USDJPY, GBPJPY, EURGBP] {
        let (a, b) =
            (back.account.contract_value_of(symbol), live.account.contract_value_of(symbol));
        assert_eq!((a.kind, bits(a.per_unit)), (b.kind, bits(b.per_unit)), "{symbol}");
    }
    assert_eq!(bits(back.account.contract_value_of(GBPJPY).per_unit), bits(10.0 * r2));
    assert_eq!(back.snapshot_state(), snap2);
    assert_eq!(state_hash(&[back.snapshot_state()]), state_hash(std::slice::from_ref(&snap2)));

    // the newer snapshot restores the new rate directly, with no round trip through the old one
    let direct = ExecutionEngine::from_snapshot(&snap2, RecordingClient::default());
    assert_eq!(bits(direct.account.contract_value_of(GBPJPY).per_unit), bits(10.0 * r2));
}

/// A mount-built engine takes the foreign symbols back with the snapshot's rate. The rate is as old as
/// the snapshot, so past the window an OPENING order is denied while a reducing one passes; a fresh
/// re-admission (what the node sends right after the boot) lifts it.
#[test]
fn restore_runtime_state_brings_the_foreign_symbols_back_stale_and_a_readmission_lifts_it() {
    let snap = foreign_engine().snapshot_state();
    let boot_ms = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 5_000;
    let mut booted = engine();
    booted.now_ms = boot_ms;
    booted.restore_runtime_state(&snap);

    for symbol in [USDJPY, GBPJPY, EURGBP, "GBPUSD"] {
        assert!(booted.accepts_symbol(symbol), "{symbol} came back");
    }
    assert!(booted.account.is_foreign_quoted(USDJPY));
    assert!(!booted.account.is_foreign_quoted("GBPUSD"));
    assert_eq!(booted.account.quote_ccy_of(GBPJPY), Some("JPY"));
    assert_eq!(bits(booted.account.quote_rate_of(USDJPY)), bits(R), "the snapshot's rate");
    assert_eq!(bits(booted.account.contract_value_of(GBPJPY).per_unit), bits(10.0 * R));
    assert_eq!(
        booted.account.quote_leg(USDJPY, boot_ms),
        (R, true),
        "stale: older than the window"
    );
    assert!(booted.account.quote_rate_table(boot_ms).1.iter().all(|row| row.stale));
    assert_eq!(size(&booted, USDJPY), Some(Q), "and the position");

    priced(&mut booted, USDJPY, 150.0, boot_ms);
    let opening = limit("o1", USDJPY, 1, 1000.0, false);
    assert_eq!(submit(&mut booted, &opening, boot_ms).as_deref(), Some("quote-rate-stale"));
    let reducing = limit("r1", USDJPY, -1, 40_000.0, true);
    assert_eq!(
        submit(&mut booted, &reducing, boot_ms),
        None,
        "a stale rate never traps a position"
    );

    // the node's re-admission, with a fresh instant: the order that was refused now goes
    let fresh = quoted(USDJPY, "JPY", R, boot_ms);
    assert_eq!(booted.admit_symbol(&fresh), AdmitOutcome::Refreshed);
    assert_eq!(booted.account.quote_leg(USDJPY, boot_ms), (R, false));
    assert_eq!(submit(&mut booted, &opening, boot_ms), None);
}

/// The window is the snapshot's rate against the engine clock at BOOT: restored just inside it, the
/// rate is still good.
#[test]
fn a_restore_inside_the_window_is_not_stale() {
    let snap = foreign_engine().snapshot_state();
    let mut booted = engine();
    booted.now_ms = T0 + DEFAULT_QUOTE_RATE_TTL_MS;
    booted.restore_runtime_state(&snap);
    let now = booted.now_ms;
    assert_eq!(booted.account.quote_leg(USDJPY, now), (R, false));
    priced(&mut booted, USDJPY, 150.0, now);
    let opening = limit("o1", USDJPY, 1, 1000.0, false);
    assert_eq!(submit(&mut booted, &opening, now), None);
}

/// The jump memory is part of the book: it survives `from_snapshot` and `restore_runtime_state`, so
/// the confirming offer after a restart is accepted, and a rate that was never offered is not.
#[test]
fn the_jump_memory_survives_a_snapshot() {
    let mut e = usdjpy_engine(R);
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", 150.0, T0 + 1)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    let snap = e.snapshot_state();
    let row = &snap.account.fx.as_ref().expect("fx").rates[0];
    assert_eq!((row.pending, row.pending_as_of_ms), (Some(150.0), Some(T0 + 1)));
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(json.contains(r#""pending":150.0"#), "{json}");
    assert!(json.contains(&format!(r#""pending_as_of_ms":{}"#, T0 + 1)), "{json}");

    // the confirming offer is a LATER observation: a second after the refused one
    let confirm = T0 + 1 + MIN_QUOTE_RATE_CONFIRM_GAP_MS;

    // from_snapshot
    let mut back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(back.snapshot_state(), snap);
    assert_eq!(
        back.admit_symbol(&quoted(USDJPY, "JPY", 150.0, T0 + 2)),
        AdmitOutcome::Refused("quote-rate-jump"),
        "one millisecond later is not another observation"
    );
    assert_eq!(back.admit_symbol(&quoted(USDJPY, "JPY", 150.0, confirm)), AdmitOutcome::Refreshed);

    // restore_runtime_state onto a mount-built engine
    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    assert_eq!(booted.snapshot_state().account.fx, snap.account.fx);
    assert_eq!(
        booted.admit_symbol(&quoted(USDJPY, "JPY", 150.0, confirm)),
        AdmitOutcome::Refreshed
    );

    // the control: an engine that never saw the first offer refuses it
    let mut fresh = usdjpy_engine(R);
    assert_eq!(
        fresh.admit_symbol(&quoted(USDJPY, "JPY", 150.0, confirm)),
        AdmitOutcome::Refused("quote-rate-jump")
    );

    // a snapshot written before the stamp existed (a `pending` with no `pending_as_of_ms`) restores
    // as NO memory: the offer that would have confirmed it is a first offer again
    let mut old = snap.clone();
    old.account.fx.as_mut().expect("fx").rates[0].pending_as_of_ms = None;
    let mut unstamped = ExecutionEngine::from_snapshot(&old, RecordingClient::default());
    assert_eq!(
        unstamped.admit_symbol(&quoted(USDJPY, "JPY", 150.0, confirm)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
}

/// An engine WITHOUT a foreign symbol serializes with no `fx` key at all, an old snapshot (without the
/// key) reads back as such an engine, and naming the account's own currency changes nothing.
#[test]
fn an_engine_without_a_foreign_symbol_serializes_with_no_fx_key() {
    let mut e = engine();
    e.admit_symbol(&plain("GBPUSD"));
    e.admit_symbol(&SymbolAdmission { quote_ccy: Some("USD".into()), ..plain("AUDUSD") });
    fold(&mut e, fill_event("p1", "GBPUSD", 1, 1000.0, 1.25));
    let snap = e.snapshot_state();
    assert!(snap.account.fx.is_none());
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(!json.contains(r#""fx""#), "{json}");
    assert!(!json.contains("account_ccy") && !json.contains("pending"), "{json}");

    // the same bytes read back (a snapshot written before the field existed is this document)
    let parsed: EngineSnapshot = serde_json::from_str(&json).expect("parses");
    assert_eq!(parsed, snap);
    assert!(
        !ExecutionEngine::from_snapshot(&parsed, RecordingClient::default())
            .account
            .has_foreign_quote()
    );

    // and the foreign engine's document differs from it only by the key and the base rows
    let foreign = serde_json::to_string(&foreign_engine().snapshot_state()).expect("serializes");
    assert!(foreign.contains(r#""fx""#));
}

/// The hash covers the book: the same admissions hash alike, and a different rate or a different
/// instant is a different state (realized PnL depends on both, so a replay must see them).
#[test]
fn the_hash_covers_the_rate_and_its_instant() {
    let hash = |e: &ExecutionEngine<RecordingClient>| state_hash(&[e.snapshot_state()]);
    let build = |rate: f64, as_of: i64| {
        let mut e = engine();
        assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", rate, as_of)), AdmitOutcome::Admitted);
        fold(&mut e, fill_event("h1", USDJPY, 1, Q, 150.0));
        e
    };
    assert_eq!(hash(&build(R, T0)), hash(&build(R, T0)), "determinism");
    assert_ne!(hash(&build(R, T0)), hash(&build(1.0 / 151.0, T0)), "the rate");
    assert_ne!(hash(&build(R, T0)), hash(&build(R, T0 + 1)), "its instant");
}

/// A snapshot that lists a foreign symbol but carries no rate for its currency (only a hand-edited one
/// can) is never valued at par by the engine `from_snapshot` builds: the symbol keeps its own unit, its
/// rate is absent (`NaN`, never a number that would pass for par) and it is stale for ever, so an
/// opening order is denied and a reducing one is not.
#[test]
fn from_snapshot_without_a_rate_row_is_stale_for_ever_and_never_par() {
    let mut snap = foreign_engine().snapshot_state();
    snap.account.fx.as_mut().expect("fx").rates.clear();
    let mut back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert!(back.account.is_foreign_quoted(USDJPY));
    assert!(back.account.quote_rate_of(USDJPY).is_nan());
    assert_eq!(back.account.quote_leg(USDJPY, T0), (1.0, true));
    assert_eq!(back.account.quote_leg(USDJPY, i64::MAX), (1.0, true));

    priced(&mut back, USDJPY, 150.0, T0);
    let opening = limit("o1", USDJPY, 1, 1000.0, false);
    assert_eq!(submit(&mut back, &opening, T0).as_deref(), Some("quote-rate-stale"));
    let reducing = limit("r1", USDJPY, -1, 40_000.0, true);
    assert_eq!(submit(&mut back, &reducing, T0), None);
}

/// The same hand-edited snapshot onto a mount-built engine (`restore_runtime_state`): the documented
/// promise (`Account::restore_fx_symbol`) is that valuing such a symbol at par is "the one thing a
/// restore must not do". A fill on it must not fold as an ordinary account-quoted symbol.
#[test]
fn a_restored_symbol_with_no_rate_row_does_not_fold_a_fill_at_par() {
    let mut snap = foreign_engine().snapshot_state();
    snap.account.fx.as_mut().expect("fx").rates.clear();
    let mut booted = engine();
    booted.restore_runtime_state(&snap);

    let outcome = fold(&mut booted, fill_event("x1", GBPJPY, 1, 100.0, 190.0));
    assert!(
        matches!(outcome, Fold::Dropped) && size(&booted, GBPJPY).is_none(),
        "a fill on a symbol the engine holds no rate for was folded: {outcome:?}, position {:?}, \
         contract value {:?}",
        size(&booted, GBPJPY),
        booted.account.contract_value_of(GBPJPY)
    );
}
