//! The MOUNTED symbol quoted in a currency other than the ledger's (decision 0183, the mounted-symbol
//! amendment): a EUR account mounted on `EURUSD`.
//!
//! The engine used to book the symbol it was started on at par in its own quote money, so a ledger could
//! carry the account's currency only when that currency was the mount's (a USD account on `EURUSD`). The
//! mount is now admitted like any other foreign-quoted symbol: `ExecutionEngine::declare_mount_quote_ccy`
//! says at assembly that it is quoted in `USD` with NO rate yet (so the engine fails closed from its first
//! instant: an opening order is denied `quote-rate-stale`, a covered reduce is not), and an
//! `AdmitSymbol` of the mounted symbol that names the currency carries the rate
//! (`ExecutionEngine::admit_symbol`'s mounted arm). Everything else, the conversion, the staleness lane,
//! the jump cap and the snapshot, is the one `crates/vike-exec/src/account/fx.rs` already holds.
//!
//! The rate `R` is EUR per ONE USD, `1/1.16`: the orientation of every rate in this engine (account units
//! per quote unit), so a EURUSD fill at 1.16 is 1.16 USD per EUR times `R` is 1 EUR per EUR.
//!
//! A USD ledger on the same mount is IDENTITY: it never declares, never reaches the judgement, and hashes
//! as it did (`an_admission_in_the_ledgers_own_money_is_identity_for_a_usd_engine`).

use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, DEFAULT_QUOTE_RATE_TTL_MS, EngineSnapshot, ExecutionEngine, Outbox,
    SymbolAdmission, state_hash,
};
use vike_model::OrderRequest;
use vike_model::OrderType;
use vike_model::events::{Event, FillEvent};

use crate::quote_currency::{
    MOUNTED, T0, USDJPY, VENUE, engine, fill_event, fold, plain, quoted, size,
};
use crate::support::EngineBuilder;

/// EUR per USD at EURUSD 1.16.
const R: f64 = 1.0 / 1.16;
const GBPUSD: &str = "GBPUSD";

fn bits(x: f64) -> u64 {
    x.to_bits()
}

/// A EUR account mounted on EURUSD, TOLD its currency and with NO declaration: what an engine is before
/// the node's assembly declares its mount foreign (and what a replay of an older journal is).
fn eur_engine() -> ExecutionEngine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    e.set_account_ccy("EUR").expect("a currency code");
    e.now_ms = T0;
    e
}

/// [`eur_engine`] with its mount declared quoted in USD, no rate yet: the engine the node's assembly builds.
fn declared() -> ExecutionEngine<RecordingClient> {
    let mut e = eur_engine();
    assert_eq!(e.declare_mount_quote_ccy("USD"), Ok(()));
    e
}

/// The declared engine holding `qty` of the mount it booked BEFORE the declaration (a fill that overtook it):
/// a position on an unrated mount. A fill that arrives after the declaration, from outside the engine, is HELD
/// (`an_external_fill_on_an_unrated_declared_mount_is_held_not_booked_at_par`), so this is the only road.
fn declared_holding(qty: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = eur_engine();
    fold(&mut e, fill_event("d1", MOUNTED, 1, qty, 1.16));
    assert_eq!(e.declare_mount_quote_ccy("USD"), Ok(()));
    e
}

/// The declared engine with the mount's first rate admitted, observed at [`T0`].
fn rated() -> ExecutionEngine<RecordingClient> {
    let mut e = declared();
    assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", R, T0)), AdmitOutcome::Mounted);
    e
}

fn limit(coid: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: MOUNTED.into(),
        side,
        qty,
        order_type: OrderType::Limit,
        price: Some(1.16),
        reduce_only,
        ..Default::default()
    }
}

/// The reason the gate denied `req` at `now_ms`, or `None` when it reached the venue client.
fn denial(
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

/// A EURUSD fill with a commission in `asset`.
fn fill(id: &'static str, side: i32, qty: f64, px: f64, commission: f64, asset: &str) -> FillEvent {
    FillEvent {
        commission,
        commission_asset: asset.into(),
        ..fill_event(id, MOUNTED, side, qty, px)
    }
}

/// Everything an admission may write, in comparable form.
fn fingerprint(e: &ExecutionEngine<RecordingClient>) -> (EngineSnapshot, Vec<String>) {
    (e.snapshot_state(), e.extra_symbols.clone())
}

#[test]
fn a_mount_admitted_in_a_foreign_currency_is_booked_in_the_ledger_currency() {
    let mut e = eur_engine();
    assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", R, T0)), AdmitOutcome::Mounted);

    // the mount is a foreign-quoted symbol now: the venue's unit times the rate, in EUR
    assert!(e.account.is_foreign_quoted(MOUNTED));
    assert_eq!(e.account.quote_ccy_of(MOUNTED), Some("USD"));
    assert_eq!(bits(e.account.contract_value_of(MOUNTED).per_unit), bits(R));
    assert_eq!(e.account.quote_leg(MOUNTED, T0), (R, false));
    assert_eq!(e.account.fx_account_ccy(), Some("EUR"));

    // ...and still the MOUNT: no extra symbol, no grid row, no change in what the engine accepts
    assert!(e.accepts_symbol(MOUNTED));
    assert!(e.extra_symbols.is_empty());
    assert!(e.gate.limits.grid_by_symbol.is_empty(), "its scalars ARE its grid");
}

#[test]
fn a_mount_declared_foreign_has_its_unit_and_no_rate_until_the_first_admission() {
    let e = declared();
    assert!(e.account.is_foreign_quoted(MOUNTED));
    assert_eq!(e.account.quote_ccy_of(MOUNTED), Some("USD"));
    // never an invented number: the row is the venue's own unit, the rate is absent and the verdict is stale
    assert_eq!(e.account.contract_value_of(MOUNTED), vike_model::ContractValue::linear(1.0));
    assert!(e.account.quote_rate_of(MOUNTED).is_nan());
    assert_eq!(e.account.quote_leg(MOUNTED, T0), (1.0, true));
    assert_eq!(e.account.quote_leg(MOUNTED, i64::MAX), (1.0, true));
    assert!(e.account.quote_rate_table(T0).1.is_empty(), "no rate row to publish");
    assert!(e.extra_symbols.is_empty() && e.gate.limits.grid_by_symbol.is_empty());

    // the first admission turns the declaration into a valuation
    let mut e = e;
    assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", R, T0)), AdmitOutcome::Mounted);
    assert_eq!(bits(e.account.contract_value_of(MOUNTED).per_unit), bits(R));
    assert_eq!(e.account.quote_leg(MOUNTED, T0), (R, false));
}

/// The fail-closed lane an unrated mount rides is the EXISTING one (`Account::quote_leg` is stale for a
/// foreign symbol with no standing rate): an OPENING order is denied `quote-rate-stale`, a covered reduce is
/// never held up, and the first rate lifts the denial.
#[test]
fn an_unrated_declared_mount_denies_an_opening_order_and_lets_a_covered_reduce_through() {
    let mut e = declared_holding(1_000.0);
    assert_eq!(size(&e, MOUNTED), Some(1_000.0));

    let opening = limit("o1", 1, 500.0, false);
    assert_eq!(denial(&mut e, &opening, T0).as_deref(), Some("quote-rate-stale"));
    let reducing = limit("r1", -1, 400.0, true);
    assert_eq!(denial(&mut e, &reducing, T0), None, "a missing rate never traps a position");

    assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", R, T0)), AdmitOutcome::Mounted);
    assert_eq!(denial(&mut e, &opening, T0), None, "the first rate lifts it");
}

#[test]
fn a_mount_rate_that_ages_out_keeps_its_valuation_and_denies_only_opening_orders() {
    let mut e = rated();
    fold(&mut e, fill_event("d1", MOUNTED, 1, 1_000.0, 1.16));
    let value = e.account.contract_value_of(MOUNTED);
    let late = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 1;

    assert_eq!(
        e.account.quote_leg(MOUNTED, late),
        (R, true),
        "stale, and the rate is the last one"
    );
    assert_eq!(e.account.contract_value_of(MOUNTED), value, "the valuation keeps the last rate");
    assert_eq!(
        denial(&mut e, &limit("o1", 1, 500.0, false), late).as_deref(),
        Some("quote-rate-stale")
    );
    assert_eq!(denial(&mut e, &limit("r1", -1, 400.0, true), late), None);

    // inside the window the same order goes
    assert_eq!(denial(&mut e, &limit("o2", 1, 500.0, false), T0 + DEFAULT_QUOTE_RATE_TTL_MS), None);
}

#[test]
fn a_mount_admission_refused_leaves_the_engine_exactly_as_it_was() {
    // never told its currency: a rate against an unknown unit is refused, and a declaration too
    let mut untold =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    untold.now_ms = T0;
    let before = fingerprint(&untold);
    assert_eq!(
        untold.admit_symbol(&quoted(MOUNTED, "USD", R, T0)),
        AdmitOutcome::Refused("account-ccy-unset")
    );
    assert_eq!(untold.declare_mount_quote_ccy("USD"), Err("account-ccy-unset"));
    assert!(fingerprint(&untold) == before);

    // a foreign currency with no rate, a stale one and one from the future
    let mut e = declared();
    let before = fingerprint(&e);
    let no_rate =
        SymbolAdmission { quote_rate: None, rate_as_of_ms: None, ..quoted(MOUNTED, "USD", R, T0) };
    assert_eq!(e.admit_symbol(&no_rate), AdmitOutcome::Refused("no-quote-rate"));
    let stale = quoted(MOUNTED, "USD", R, T0 - DEFAULT_QUOTE_RATE_TTL_MS - 1);
    assert_eq!(e.admit_symbol(&stale), AdmitOutcome::Refused("quote-rate-stale"));
    let future = quoted(MOUNTED, "USD", R, T0 + 10 * DEFAULT_QUOTE_RATE_TTL_MS);
    assert_eq!(e.admit_symbol(&future), AdmitOutcome::Refused("quote-rate-future"));
    assert!(fingerprint(&e) == before, "a refusal wrote nothing");
    assert_eq!(e.account.quote_leg(MOUNTED, T0), (1.0, true), "and the mount is still unrated");
}

/// The jump cap and its memory are the ordinary ones: a refresh that moves the rate by more than a step
/// is refused, and the same rate from a LATER observation confirms it.
#[test]
fn a_mount_rate_refresh_runs_through_the_ordinary_jump_cap() {
    let mut e = rated();
    // a rate that halves the EUR value of a dollar in one step (outside the cap, `MAX_QUOTE_RATE_STEP`)
    assert_eq!(
        e.admit_symbol(&quoted(MOUNTED, "USD", 0.5, T0 + 1)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    assert_eq!(bits(e.account.quote_rate_of(MOUNTED)), bits(R), "the standing rate is kept");
    // a small move is taken
    e.now_ms = T0 + 30_000;
    assert_eq!(
        e.admit_symbol(&quoted(MOUNTED, "USD", 1.0 / 1.17, T0 + 30_000)),
        AdmitOutcome::Mounted
    );
    assert_eq!(bits(e.account.quote_rate_of(MOUNTED)), bits(1.0 / 1.17));
}

/// A mount the engine holds as foreign-quoted is never read as the ledger's own money: an admission that
/// names no currency is refused, and one that names another currency is refused as a change.
#[test]
fn a_foreign_mount_is_never_admitted_without_its_currency_or_under_another_one() {
    let mut e = rated();
    let before = fingerprint(&e);
    assert_eq!(e.admit_symbol(&plain(MOUNTED)), AdmitOutcome::Refused("quote-ccy-missing"));
    assert_eq!(
        e.admit_symbol(&quoted(MOUNTED, "JPY", 1.0 / 168.0, T0)),
        AdmitOutcome::Refused("quote-ccy-changed")
    );
    // the ledger's own currency on a mount held foreign is a change too
    assert_eq!(
        e.admit_symbol(&quoted(MOUNTED, "EUR", 1.0, T0)),
        AdmitOutcome::Refused("quote-ccy-changed")
    );
    assert!(fingerprint(&e) == before);
}

/// One standing rate per quote currency: a USD-quoted symbol admitted beside the declared mount rates the
/// mount at once, and the mount's own refresh moves that symbol's value too.
#[test]
fn the_mount_shares_its_currencys_rate_with_every_other_usd_quoted_symbol() {
    let mut e = declared();
    assert_eq!(e.admit_symbol(&quoted(GBPUSD, "USD", R, T0)), AdmitOutcome::Admitted);
    assert_eq!(
        bits(e.account.contract_value_of(MOUNTED).per_unit),
        bits(R),
        "rated by its sibling"
    );
    assert_eq!(e.account.quote_leg(MOUNTED, T0), (R, false));

    let r2 = 1.0 / 1.17;
    e.now_ms = T0 + 30_000;
    assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", r2, T0 + 30_000)), AdmitOutcome::Mounted);
    assert_eq!(bits(e.account.contract_value_of(GBPUSD).per_unit), bits(r2), "and the other way");
}

#[test]
fn declaring_the_mount_is_idempotent_and_refuses_what_it_cannot_say() {
    let mut e = eur_engine();
    // the mount in the ledger's own money needs no rate: nothing is declared
    assert_eq!(e.declare_mount_quote_ccy(" eur "), Ok(()));
    assert!(!e.account.has_foreign_quote());
    assert_eq!(e.declare_mount_quote_ccy("  "), Err("quote-ccy-missing"));

    assert_eq!(e.declare_mount_quote_ccy("usd"), Ok(()));
    let once = e.snapshot_state();
    assert_eq!(e.declare_mount_quote_ccy("USD"), Ok(()));
    assert!(e.snapshot_state() == once, "declaring again changes nothing");
    assert_eq!(e.account.quote_ccy_of(MOUNTED), Some("USD"));
    assert_eq!(e.declare_mount_quote_ccy("JPY"), Err("quote-ccy-changed"));
}

/// `account-ccy-changed` still refuses re-telling once the ledger holds the mount as foreign: every rate in
/// the book is in the old unit.
#[test]
fn the_account_currency_cannot_be_retold_once_the_mount_is_declared() {
    let mut e = declared();
    assert_eq!(e.set_account_ccy("USD"), Err("account-ccy-changed"));
    assert_eq!(e.set_account_ccy("GBP"), Err("account-ccy-changed"));
    assert_eq!(e.set_account_ccy("eur"), Ok(()), "telling it what it has is a no-op");
    assert_eq!(e.quote_asset, "EUR");
}

/// Realized PnL of a round trip on the mount is the QUOTE PnL times the rate in force when the closing fill
/// folds (the margin-account convention of every foreign-quoted symbol); the unrealized one moves with the
/// rate now; a commission in the account's own currency is booked as stated.
#[test]
fn a_round_trip_on_the_mount_realizes_the_quote_pnl_times_the_closing_rate() {
    let mut e = rated();
    fold(&mut e, fill("b1", 1, 100_000.0, 1.1000, 0.5, "EUR"));
    // the rate moves between the fills: the close takes the one in force then
    let r2 = 1.0 / 1.1200;
    e.now_ms = T0 + 30_000;
    assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", r2, T0 + 30_000)), AdmitOutcome::Mounted);
    fold(&mut e, fill("s1", -1, 100_000.0, 1.1200, 0.5, "EUR"));

    let quote_pnl = 100_000.0 * (1.1200 - 1.1000); // 2000 USD
    let want = quote_pnl * r2;
    assert!(
        (e.account.realized_pnl - want).abs() <= 1e-9 * want,
        "realized {} want {want} (the quote PnL {quote_pnl} times {r2})",
        e.account.realized_pnl
    );
    assert_eq!(size(&e, MOUNTED), Some(0.0), "flat");
    assert_eq!(bits(e.account.fees_paid), bits(1.0), "the fee is in EUR already: booked as stated");
}

/// A fee in USD, the mount's own quote currency, is converted by the mount's rate (the rule any foreign
/// symbol follows), and one that names no asset is the account's.
#[test]
fn a_commission_in_the_mounts_quote_currency_is_converted_and_an_unnamed_one_is_the_accounts() {
    let mut e = rated();
    fold(&mut e, fill("f1", 1, 1_000.0, 1.16, 2.0, "USD"));
    assert_eq!(bits(e.account.fees_paid), bits(2.0 * R), "2 USD in EUR");
    fold(&mut e, fill("f2", 1, 1_000.0, 1.16, 3.0, ""));
    assert_eq!(bits(e.account.fees_paid), bits(2.0 * R + 3.0));
}

#[test]
fn a_mount_engine_survives_the_snapshot_round_trip_declared_and_rated() {
    for (label, mut e) in [("declared", declared_holding(2_500.0)), ("rated", rated())] {
        if label == "rated" {
            fold(&mut e, fill_event("p1", MOUNTED, 1, 2_500.0, 1.16));
        }
        let snap = e.snapshot_state();
        let fx = snap.account.fx.as_ref().expect("the mount is in the fx table");
        assert_eq!(fx.account_ccy, "EUR", "{label}");
        assert_eq!(fx.symbols, vec![(MOUNTED.to_string(), "USD".to_string())], "{label}");
        assert_eq!(fx.rates.len(), usize::from(label == "rated"), "{label}");
        // the BASE is what the snapshot keeps, never the product
        assert_eq!(snap.account.mult, vec![(MOUNTED.to_string(), 1.0)], "{label}");

        let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
        assert!(back.snapshot_state() == snap, "{label}");
        assert_eq!(state_hash(&[back.snapshot_state()]), state_hash(std::slice::from_ref(&snap)));
        assert_eq!(
            back.account.quote_leg(MOUNTED, T0),
            e.account.quote_leg(MOUNTED, T0),
            "{label}"
        );
        assert_eq!(
            bits(back.account.contract_value_of(MOUNTED).per_unit),
            bits(e.account.contract_value_of(MOUNTED).per_unit),
            "{label}"
        );
        // through the JSON the journal writes
        let json = serde_json::to_string(&snap).expect("serializes");
        let parsed: EngineSnapshot = serde_json::from_str(&json).expect("parses");
        assert!(parsed == snap, "{label}");
    }
}

/// A replay folds the same inputs over the snapshot the live engine started from and lands on the same
/// hash, rate refreshes and fills included: the mount's rate rides the journaled admission.
#[test]
fn replaying_the_journaled_inputs_over_the_starting_snapshot_reproduces_the_state_hash() {
    let mut live = declared();
    let start = live.snapshot_state();
    let mut replay = ExecutionEngine::from_snapshot(&start, RecordingClient::default());
    for e in [&mut live, &mut replay] {
        e.now_ms = T0;
        assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", R, T0)), AdmitOutcome::Mounted);
        fold(e, fill("a1", 1, 100_000.0, 1.1600, 0.0, ""));
        e.now_ms = T0 + 30_000;
        assert_eq!(
            e.admit_symbol(&quoted(MOUNTED, "USD", 1.0 / 1.15, T0 + 30_000)),
            AdmitOutcome::Mounted
        );
        assert_eq!(
            e.admit_symbol(&quoted(GBPUSD, "USD", 1.0 / 1.15, T0 + 30_000)),
            AdmitOutcome::Admitted
        );
        fold(e, fill("a2", -1, 40_000.0, 1.1500, 1.0, "USD"));
    }
    // the run really exercised the foreign mount: the closing fill booked its quote PnL at the rate in force
    assert!(live.account.is_foreign_quoted(MOUNTED) && replay.account.is_foreign_quoted(MOUNTED));
    let want = 40_000.0 * (1.1500 - 1.1600) * (1.0 / 1.15);
    assert!(
        (live.account.realized_pnl - want).abs() <= 1e-9 * want.abs(),
        "{}",
        live.account.realized_pnl
    );
    assert_eq!(
        state_hash(&[live.snapshot_state()]),
        state_hash(&[replay.snapshot_state()]),
        "live and replay agree"
    );
    assert!(live.snapshot_state() == replay.snapshot_state());
}

/// The boot restore (`restore_runtime_state` onto an engine its mount built and declared) brings the
/// mount's rate back with the snapshot, as old as the snapshot says: a restored position is valued at
/// the last rate instead of at par, an opening order is denied until the node admits the mount again, and
/// a reduce is not.
#[test]
fn restore_runtime_state_brings_the_mounts_rate_back_stale_and_a_readmission_lifts_it() {
    let mut src = rated();
    fold(&mut src, fill_event("m1", MOUNTED, 1, 10_000.0, 1.16));
    let snap = src.snapshot_state();

    let boot_ms = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 5_000;
    let mut booted = declared();
    booted.now_ms = boot_ms;
    booted.restore_runtime_state(&snap);
    assert_eq!(size(&booted, MOUNTED), Some(10_000.0));
    assert_eq!(bits(booted.account.quote_rate_of(MOUNTED)), bits(R), "the snapshot's rate");
    assert_eq!(
        bits(booted.account.contract_value_of(MOUNTED).per_unit),
        bits(R),
        "valued, not at par"
    );
    assert_eq!(booted.account.quote_leg(MOUNTED, boot_ms), (R, true), "as old as the snapshot");
    assert!(booted.account.quote_rate_table(boot_ms).1.iter().all(|row| row.stale));

    let opening = limit("o1", 1, 500.0, false);
    assert_eq!(denial(&mut booted, &opening, boot_ms).as_deref(), Some("quote-rate-stale"));
    assert_eq!(denial(&mut booted, &limit("r1", -1, 4_000.0, true), boot_ms), None);

    assert_eq!(booted.admit_symbol(&quoted(MOUNTED, "USD", R, boot_ms)), AdmitOutcome::Mounted);
    assert_eq!(booted.account.quote_leg(MOUNTED, boot_ms), (R, false));
    assert_eq!(denial(&mut booted, &opening, boot_ms), None);
}

/// What the restore never does: invent a rate for a mount the snapshot holds none for, take the snapshot's
/// currency for a mount declared under another, or touch an engine that holds its mount in its own money.
#[test]
fn the_restore_of_a_mounts_rate_takes_nothing_it_was_not_given() {
    // a snapshot that never had the mount foreign (a USD engine's): the declared mount stays unrated
    let usd = engine().snapshot_state();
    let mut booted = declared();
    booted.restore_runtime_state(&usd);
    assert_eq!(booted.account.quote_leg(MOUNTED, T0), (1.0, true));
    assert!(booted.account.quote_rate_of(MOUNTED).is_nan());

    // a snapshot of the DECLARED, never-rated mount: still no rate
    let unrated = declared().snapshot_state();
    let mut booted = declared();
    booted.restore_runtime_state(&unrated);
    assert_eq!(booted.account.quote_leg(MOUNTED, T0), (1.0, true));

    // the mount under another currency in the snapshot: not the declared one's rate
    let mut other = eur_engine();
    other.declare_mount_quote_ccy("JPY").expect("declared");
    other.admit_symbol(&quoted(MOUNTED, "JPY", 1.0 / 168.0, T0));
    let mut booted = declared();
    booted.restore_runtime_state(&other.snapshot_state());
    assert_eq!(booted.account.quote_ccy_of(MOUNTED), Some("USD"));
    assert!(booted.account.quote_rate_of(MOUNTED).is_nan());

    // an engine whose mount is in its own money restores no mount rate and gains no fx book
    let mut usd_engine = engine();
    usd_engine.restore_runtime_state(&rated().snapshot_state());
    assert!(!usd_engine.account.is_foreign_quoted(MOUNTED));
    assert!(!usd_engine.account.has_foreign_quote());
}

/// IDENTITY for a USD ledger on the same mount: an admission that names the ledger's own currency, or none,
/// is what it always was, and the engine hashes as an engine that never heard it.
#[test]
fn an_admission_in_the_ledgers_own_money_is_identity_for_a_usd_engine() {
    let mut heard = engine();
    let untouched = engine();
    for a in [plain(MOUNTED), quoted(MOUNTED, "USD", 1.0, T0), quoted(MOUNTED, "usd", 1.0, T0)] {
        assert_eq!(heard.admit_symbol(&a), AdmitOutcome::Mounted, "{a:?}");
    }
    assert!(!heard.account.has_foreign_quote());
    assert!(heard.snapshot_state() == untouched.snapshot_state());
    assert_eq!(state_hash(&[heard.snapshot_state()]), state_hash(&[untouched.snapshot_state()]));
    let json = serde_json::to_string(&heard.snapshot_state()).expect("serializes");
    assert!(!json.contains(r#""fx""#), "a USD engine writes no fx key: {json}");

    // and declaring its mount (the node declares only a foreign one) is a no-op
    let mut declared_usd = engine();
    assert_eq!(declared_usd.declare_mount_quote_ccy("USD"), Ok(()));
    assert!(declared_usd.snapshot_state() == untouched.snapshot_state());
}

/// A fill that came from OUTSIDE the engine (a venue-UI trade: no order of its registry) on a mount declared
/// foreign and not yet rated is HELD: folding it would book the venue's own unit as the ledger's money, wrong
/// by the whole rate. Nothing moves, the `trade_id` is not burned, and the same fill folds at the rate once the
/// first admission has landed one.
#[test]
fn an_external_fill_on_an_unrated_declared_mount_is_held_not_booked_at_par() {
    let mut e = declared();
    let before = fingerprint(&e);
    let ui_trade = fill_event("ui1", MOUNTED, 1, 1_000.0, 1.16);

    assert_eq!(fold(&mut e, ui_trade.clone()), vike_exec::Fold::Dropped, "held");
    assert_eq!(size(&e, MOUNTED), None, "no position was booked at par");
    assert!(fingerprint(&e) == before, "nothing moved: no position, balance, fee or trade id");

    // once the mount is rated, the very same fill (a redelivery, or reconcile's) folds at the rate
    assert_eq!(e.admit_symbol(&quoted(MOUNTED, "USD", R, T0)), AdmitOutcome::Mounted);
    assert_eq!(fold(&mut e, ui_trade), vike_exec::Fold::Applied);
    assert_eq!(size(&e, MOUNTED), Some(1_000.0));
    assert_eq!(bits(e.account.contract_value_of(MOUNTED).per_unit), bits(R));
}

/// The guard asks only about fills the engine does not own: a fill of an order its registry holds (a covered
/// reduce passes the gate on an unrated mount) is folded, and a USD ledger, which never declares its mount,
/// folds an outside fill as it always did.
#[test]
fn a_fill_of_an_order_the_engine_owns_still_folds_on_an_unrated_mount() {
    let mut e = declared_holding(1_000.0);
    assert_eq!(denial(&mut e, &limit("r1", -1, 400.0, true), T0), None, "a covered reduce passes");
    let own =
        FillEvent { client_order_id: "r1".into(), ..fill_event("x1", MOUNTED, -1, 400.0, 1.17) };
    assert_eq!(fold(&mut e, own), vike_exec::Fold::Applied);
    assert_eq!(size(&e, MOUNTED), Some(600.0));

    // a fill on the mount from nowhere is still held while it stays unrated
    let outside = fill_event("x2", MOUNTED, -1, 100.0, 1.17);
    assert_eq!(fold(&mut e, outside), vike_exec::Fold::Dropped);
    assert_eq!(size(&e, MOUNTED), Some(600.0));

    let mut usd = engine();
    assert_eq!(fold(&mut usd, fill_event("u1", MOUNTED, 1, 100.0, 1.16)), vike_exec::Fold::Applied);
}

/// `Account::quote_unrated` is the declared-and-not-rated state and nothing else: a rated mount, a symbol
/// of another currency that has its rate and an account with no foreign symbol are all `false`.
#[test]
fn only_a_declared_symbol_with_no_rate_row_is_unrated() {
    assert!(declared().account.quote_unrated(MOUNTED));
    assert!(!declared().account.quote_unrated("NOSUCH"), "a symbol the book does not hold");
    assert!(!rated().account.quote_unrated(MOUNTED));
    assert!(!engine().account.quote_unrated(MOUNTED), "no foreign-quoted symbol at all");

    // a second currency with a rate beside the unrated mount: the mount alone is unrated
    let mut e = declared();
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", 1.0 / 150.0, T0)), AdmitOutcome::Admitted);
    assert!(e.account.quote_unrated(MOUNTED));
    assert!(!e.account.quote_unrated(USDJPY));
    // two symbols sharing ONE rated currency: neither is unrated (the cheap length test must not mislead)
    let mut both = rated();
    assert_eq!(both.admit_symbol(&quoted(GBPUSD, "USD", R, T0)), AdmitOutcome::Admitted);
    assert!(!both.account.quote_unrated(MOUNTED) && !both.account.quote_unrated(GBPUSD));
}

/// `QuoteRateView::held` says whether a STALE rate is valuing an open position: a stale row of a currency in
/// which the account holds nothing is not held (an admitted pair nobody trades ages out for ever), a fresh
/// row is never looked at, and a closed position holds nothing.
#[test]
fn a_stale_rate_row_says_whether_a_position_is_open_in_its_currency() {
    let late = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 1;
    let row = |e: &ExecutionEngine<RecordingClient>, ccy: &str, at: i64| {
        let (_, rows) = e.account.quote_rate_table(at);
        rows.into_iter().find(|r| r.ccy == ccy).unwrap_or_else(|| panic!("no {ccy} row"))
    };

    let mut e = rated();
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", 1.0 / 150.0, T0)), AdmitOutcome::Admitted);
    fold(&mut e, fill_event("h1", MOUNTED, 1, 1_000.0, 1.16));

    // fresh: no flag, whatever is held
    let fresh = row(&e, "USD", T0);
    assert!(!fresh.stale && !fresh.held);
    // stale: the USD row is valuing the EURUSD position, the JPY row values nothing
    let usd = row(&e, "USD", late);
    assert!(usd.stale && usd.held, "{usd:?}");
    let jpy = row(&e, "JPY", late);
    assert!(jpy.stale && !jpy.held, "{jpy:?}");

    // closed: stale and held no more
    fold(&mut e, fill_event("h2", MOUNTED, -1, 1_000.0, 1.17));
    let closed = row(&e, "USD", late);
    assert!(closed.stale && !closed.held, "{closed:?}");
}
