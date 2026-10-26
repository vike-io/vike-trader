//! The defects an independent review of the multi-currency engine half found, one test group each. Every
//! test here failed on the branch the review read (`feat/multicurrency-phase1-core`) and passes on the
//! fix; the money sums themselves are pinned in `quote_money.rs`, the admission table in
//! `quote_currency.rs`.
//!
//! * F1: the jump guard confirmed itself (two symbols of one currency in one refresh batch, or one
//!   command delivered twice, "sent the same rate twice"; and the memory never expired).
//! * F2: a rate stamped in the FUTURE pinned the rate (only the past was bounded).
//! * F4: the account's currency was a constant nobody set, and a foreign symbol was admitted against it.
//!
//! (F3, the reconcile balance lane, lives in `vike-core`; F5, the margin row, in `quote_margin.rs`.)

use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, DEFAULT_QUOTE_RATE_TTL_MS, EngineSnapshot, EventHandler, ExecutionEngine,
    MIN_QUOTE_RATE_CONFIRM_GAP_MS, Outbox, QUOTE_RATE_FUTURE_SKEW_MS, SymbolAdmission,
};
use vike_model::events::{AccountState, Event, FillEvent};

use crate::quote_currency::{
    GBPJPY, MOUNTED, T0, USDJPY, VENUE, engine, fill_event, fold, plain, quoted, usdjpy_engine,
};
use crate::support::EngineBuilder;

const R: f64 = 1.0 / 150.0;
/// The same pair quoted the wrong way up: a 22,500x step from `R`.
const FLIPPED: f64 = 150.0;
const GAP: i64 = MIN_QUOTE_RATE_CONFIRM_GAP_MS;

fn bits(x: f64) -> u64 {
    x.to_bits()
}

/// USDJPY and GBPJPY, both JPY, both at `R`, observed at [`T0`].
fn two_jpy_symbols() -> ExecutionEngine<RecordingClient> {
    let mut e = usdjpy_engine(R);
    assert_eq!(e.admit_symbol(&quoted(GBPJPY, "JPY", R, T0)), AdmitOutcome::Admitted);
    e
}

/// Both JPY symbols are valued at `rate`, bit for bit.
fn valued_at(e: &ExecutionEngine<RecordingClient>, rate: f64) {
    for symbol in [USDJPY, GBPJPY] {
        assert_eq!(bits(e.account.quote_rate_of(symbol)), bits(rate), "{symbol}");
        assert_eq!(bits(e.account.contract_value_of(symbol).per_unit), bits(rate), "{symbol}");
    }
}

// ---- F1: the jump guard cannot confirm itself --------------------------------------------------------

/// THE REVIEW'S SCENARIO. One refresh batch carries the flipped pair (150, where 1/150 stands) for every
/// symbol of the currency under ONE stamp. The first symbol is refused as a jump and remembered; the
/// second carries the same rate and used to read as "the same rate sent again": both were accepted, a
/// 22,500x error. One stamp is one observation, so neither is.
#[test]
fn two_symbols_of_one_currency_in_one_batch_cannot_confirm_each_others_flipped_rate() {
    let mut e = two_jpy_symbols();
    let batch = T0 + GAP;
    e.now_ms = batch;

    let refuse = |e: &mut ExecutionEngine<RecordingClient>, symbol: &str| {
        e.admit_symbol(&quoted(symbol, "JPY", FLIPPED, batch))
    };
    assert_eq!(refuse(&mut e, USDJPY), AdmitOutcome::Refused("quote-rate-jump"));
    assert_eq!(refuse(&mut e, GBPJPY), AdmitOutcome::Refused("quote-rate-jump"), "not confirmed");
    // the batch delivered again, in either order: still one observation
    assert_eq!(refuse(&mut e, GBPJPY), AdmitOutcome::Refused("quote-rate-jump"));
    assert_eq!(refuse(&mut e, USDJPY), AdmitOutcome::Refused("quote-rate-jump"));
    valued_at(&e, R);
}

/// A re-delivered command (the same admission, the same stamp) never confirms itself, however many times
/// it arrives.
#[test]
fn a_re_delivered_command_never_confirms_itself() {
    let mut e = usdjpy_engine(R);
    e.now_ms = T0 + GAP;
    let a = quoted(USDJPY, "JPY", FLIPPED, T0 + GAP);
    for _ in 0..8 {
        assert_eq!(e.admit_symbol(&a), AdmitOutcome::Refused("quote-rate-jump"));
    }
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(R));
}

/// A confirming offer is a LATER observation: at least the minimum gap after the refused one. One
/// millisecond less is refused, and repeating the same rate inside the gap does not push the confirming
/// time out (the memory is the earliest observation of the rate).
#[test]
fn a_later_distinct_observation_confirms_and_one_inside_the_gap_does_not() {
    let mut e = two_jpy_symbols();
    let first = T0 + GAP;
    e.now_ms = first;
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, first)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    // inside the gap, twice
    for dt in [1, GAP - 1] {
        e.now_ms = first + dt;
        assert_eq!(
            e.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, first + dt)),
            AdmitOutcome::Refused("quote-rate-jump"),
            "{dt} ms after is not another observation"
        );
    }
    valued_at(&e, R);
    // exactly the gap after the REFUSED one: confirmed, for the whole currency
    e.now_ms = first + GAP;
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, first + GAP)),
        AdmitOutcome::Refreshed
    );
    valued_at(&e, FLIPPED);
}

/// The memory of a refused jump expires with the staleness window: a confirmation that arrives after it
/// is a FIRST offer again (and becomes the new memory), while one at exactly the window still counts.
#[test]
fn the_memory_of_a_refused_jump_expires() {
    // at exactly the window: the memory still stands
    let mut at_edge = usdjpy_engine(R);
    let refused = T0 + GAP;
    at_edge.now_ms = refused;
    assert_eq!(
        at_edge.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, refused)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    let edge = refused + DEFAULT_QUOTE_RATE_TTL_MS;
    at_edge.now_ms = edge;
    assert_eq!(
        at_edge.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, edge)),
        AdmitOutcome::Refreshed
    );

    // one millisecond past it: expired
    let mut late = usdjpy_engine(R);
    late.now_ms = refused;
    assert_eq!(
        late.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, refused)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    let past = edge + 1;
    late.now_ms = past;
    assert_eq!(
        late.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, past)),
        AdmitOutcome::Refused("quote-rate-jump"),
        "the memory is older than the window"
    );
    assert_eq!(bits(late.account.quote_rate_of(USDJPY)), bits(R));
    // ...and that offer is the memory now: the next distinct observation confirms it
    late.now_ms = past + GAP;
    assert_eq!(
        late.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, past + GAP)),
        AdmitOutcome::Refreshed
    );
}

/// The currencies are separate memories: a JPY jump is not confirmed by a GBP one, whatever the rates.
#[test]
fn a_jump_in_one_currency_is_not_confirmed_by_another() {
    let mut e = usdjpy_engine(R);
    assert_eq!(e.admit_symbol(&quoted("EURGBP", "GBP", 1.35, T0)), AdmitOutcome::Admitted);
    e.now_ms = T0 + GAP;
    let stamp = T0 + GAP;
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, stamp)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    e.now_ms = stamp + GAP;
    // GBP quoted at the flipped JPY figure is its own jump, not a confirmation of the yen one
    assert_eq!(
        e.admit_symbol(&quoted("EURGBP", "GBP", FLIPPED, stamp + GAP)),
        AdmitOutcome::Refused("quote-rate-jump")
    );
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(R));
    assert_eq!(bits(e.account.quote_rate_of("EURGBP")), bits(1.35));
}

// ---- F2: a stamp from the future pins nothing --------------------------------------------------------

/// A rate stamped ahead of the engine's clock by more than the skew is refused, whatever the unit it was
/// stamped in: a microsecond or nanosecond epoch (1,000x and 1,000,000x too large), `i64::MAX`. At
/// exactly the skew it is taken.
#[test]
fn a_stamp_from_the_future_is_refused_beyond_the_skew_and_taken_at_it() {
    let skew = QUOTE_RATE_FUTURE_SKEW_MS;
    let mut e = engine();
    let before = e.snapshot_state();
    for stamp in [T0 + skew + 1, T0 * 1_000, T0 * 1_000_000, i64::MAX - 1, i64::MAX] {
        assert_eq!(
            e.admit_symbol(&quoted(USDJPY, "JPY", R, stamp)),
            AdmitOutcome::Refused("quote-rate-future"),
            "stamp {stamp}"
        );
    }
    assert_eq!(e.snapshot_state(), before, "refused: nothing written");
    assert!(!e.accepts_symbol(USDJPY) && !e.account.has_foreign_quote());

    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", R, T0 + skew)), AdmitOutcome::Admitted);
}

/// THE PIN. A future-dated offer used to be accepted, and then every real refresh was OLDER than it:
/// the newer-wins rule kept the pinned rate for ever, the jump guard was skipped, and `saturating_sub`
/// called it never stale. Refused at the door, the standing rate and its stamp are untouched and the next
/// honest refresh is taken.
#[test]
fn a_future_dated_offer_cannot_pin_the_rate() {
    let r2 = 1.0 / 155.0;
    let mut e = usdjpy_engine(R);
    for stamp in [i64::MAX, T0 * 1_000_000, T0 + QUOTE_RATE_FUTURE_SKEW_MS + 1] {
        assert_eq!(
            e.admit_symbol(&quoted(USDJPY, "JPY", r2, stamp)),
            AdmitOutcome::Refused("quote-rate-future")
        );
    }
    assert_eq!(e.account.quote_rate_table(T0).1[0].as_of_ms, T0, "the standing stamp is untouched");
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(R));

    e.now_ms = T0 + 10;
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", r2, T0 + 10)), AdmitOutcome::Refreshed);
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(r2), "the real refresh is taken");

    // a future-dated jump does not get past the guard through the back door either
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", FLIPPED, i64::MAX)),
        AdmitOutcome::Refused("quote-rate-future")
    );
    assert_eq!(bits(e.account.quote_rate_of(USDJPY)), bits(r2));
}

/// A standing rate whose stamp is ahead of the clock by more than the skew (the clock stepped back since
/// the admission) is stale to the gate and in the published table, not "never stale". Inside the skew it
/// is fresh.
#[test]
fn a_standing_stamp_ahead_of_the_clock_is_stale() {
    let skew = QUOTE_RATE_FUTURE_SKEW_MS;
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", R, T0 + skew)), AdmitOutcome::Admitted);
    assert_eq!(e.account.quote_leg(USDJPY, T0), (R, false), "exactly the skew ahead is fresh");
    assert_eq!(e.account.quote_leg(USDJPY, T0 - 1), (R, true), "one ms more is not a measurement");
    assert!(e.account.quote_rate_table(T0 - 1).1[0].stale);
    assert!(!e.account.quote_rate_table(T0).1[0].stale);
}

/// A snapshot that carries a stamp the snapshot's own clock had not reached (a hand-edited one, or one
/// written by a build that admitted such a stamp) is restored with the rate kept and the stamp already
/// stale: the position is still valued, an opening order is denied until the node admits the symbol
/// again with a real stamp. A stamp inside the skew is kept as it is. Both restore doors.
#[test]
fn a_restored_stamp_from_the_future_is_stale_and_one_inside_the_skew_is_kept() {
    let skew = QUOTE_RATE_FUTURE_SKEW_MS;
    let base = usdjpy_engine(R).snapshot_state();
    assert_eq!(base.now_ms, T0, "the snapshot's own clock");

    let with_stamp = |stamp: i64| {
        let mut snap = base.clone();
        let row = &mut snap.account.fx.as_mut().expect("fx").rates[0];
        row.as_of_ms = stamp;
        row.pending = Some(FLIPPED);
        row.pending_as_of_ms = Some(stamp);
        snap
    };
    let stale = |e: &ExecutionEngine<RecordingClient>| {
        let (rate, stale) = e.account.quote_leg(USDJPY, T0);
        assert_eq!(bits(rate), bits(R), "the rate itself is kept");
        stale
    };
    let memory = |e: &ExecutionEngine<RecordingClient>| {
        e.snapshot_state().account.fx.expect("fx").rates[0].pending_as_of_ms
    };

    for stamp in [i64::MAX, T0 * 1_000_000, T0 + skew + 1] {
        let snap = with_stamp(stamp);
        let by_snapshot = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
        assert!(stale(&by_snapshot), "from_snapshot, stamp {stamp}");
        assert_eq!(memory(&by_snapshot), None, "a jump memory from the future is dropped");

        let mut booted = engine();
        booted.restore_runtime_state(&snap);
        assert!(stale(&booted), "restore_runtime_state, stamp {stamp}");
        assert_eq!(memory(&booted), None);
    }

    let inside = with_stamp(T0 + skew);
    let by_snapshot = ExecutionEngine::from_snapshot(&inside, RecordingClient::default());
    assert!(!stale(&by_snapshot));
    assert_eq!(by_snapshot.snapshot_state(), inside, "an honest snapshot restores byte for byte");
    let mut booted = engine();
    booted.restore_runtime_state(&inside);
    assert!(!stale(&booted));
    assert_eq!(memory(&booted), Some(T0 + skew));
}

// ---- F4: the account's currency is told, not guessed -------------------------------------------------

/// An engine nobody told its currency still has the constructor's `"USDT"`, and a rate "account units
/// per JPY" against that label is a number in an unknown unit: three behaviours key on it and fail
/// quietly. The foreign admission is refused until the engine is told; an account-quoted one is not
/// held up.
#[test]
fn an_engine_never_told_its_currency_refuses_a_foreign_admission() {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    e.now_ms = T0;
    assert!(!e.account_ccy_known());
    assert_eq!(e.quote_asset, "USDT", "the constructor's guess");

    let before = e.snapshot_state();
    assert_eq!(
        e.admit_symbol(&quoted(USDJPY, "JPY", R, T0)),
        AdmitOutcome::Refused("account-ccy-unset")
    );
    assert_eq!(e.snapshot_state(), before, "refused: nothing written");
    assert_eq!(e.admit_symbol(&plain("GBPUSD")), AdmitOutcome::Admitted, "par is not held up");

    // told (the node calls this at assembly): taken, and the book is expressed in it
    assert_eq!(e.set_account_ccy(" usd "), Ok(()));
    assert_eq!(e.quote_asset, "USD");
    assert!(e.account_ccy_known());
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", R, T0)), AdmitOutcome::Admitted);
    assert_eq!(e.account.fx_account_ccy(), Some("USD"));
}

/// The setter refuses what would relabel a standing rate book, and a code that names nothing.
#[test]
fn the_setter_refuses_an_empty_code_and_a_relabelling_of_a_standing_book() {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    for blank in ["", "   "] {
        assert_eq!(e.set_account_ccy(blank), Err("account-ccy-empty"));
    }
    assert!(!e.account_ccy_known() && e.quote_asset == "USDT", "nothing changed");

    let mut held = usdjpy_engine(R);
    assert_eq!(held.set_account_ccy("EUR"), Err("account-ccy-changed"));
    assert_eq!(held.quote_asset, "USD");
    assert_eq!(held.set_account_ccy("usd"), Ok(()), "the currency it already has is a no-op");
    assert_eq!(held.account.fx_account_ccy(), Some("USD"));
}

/// Whether the engine was told is a function of `quote_asset` and ONE snapshotted flag, so a replay from
/// a `Snap` takes the verdict the live engine took, and no engine that was never told (or told a
/// currency other than the constructor's own spelling) changes a byte of its canonical JSON.
#[test]
fn being_told_rides_the_snapshot_only_where_quote_asset_cannot_say_it() {
    let json_of = |e: &ExecutionEngine<RecordingClient>| {
        serde_json::to_string(&e.snapshot_state()).expect("serializes")
    };
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    e.now_ms = T0;
    assert!(!json_of(&e).contains("account_ccy_set"), "never told: no key");

    // told a currency that is not the guess: known from `quote_asset` alone, no key
    let mut usd =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    usd.set_account_ccy("USD").expect("a code");
    assert!(usd.account_ccy_known());
    assert!(!json_of(&usd).contains("account_ccy_set"), "told USD: no key, the hash is today's");
    assert!(
        ExecutionEngine::from_snapshot(&usd.snapshot_state(), RecordingClient::default())
            .account_ccy_known()
    );

    // told the guess's own spelling: the one case that needs the flag
    e.set_account_ccy("usdt").expect("a code");
    assert!(e.account_ccy_known());
    let snap = e.snapshot_state();
    assert!(snap.account_ccy_set);
    assert!(json_of(&e).contains(r#""account_ccy_set":true"#));
    let parsed: EngineSnapshot = serde_json::from_str(&json_of(&e)).expect("reads back");
    let mut back = ExecutionEngine::from_snapshot(&parsed, RecordingClient::default());
    assert!(back.account_ccy_known(), "the replay's engine took the same verdict");
    assert_eq!(back.snapshot_state(), snap);
    assert_eq!(back.admit_symbol(&quoted(USDJPY, "JPY", R, T0)), AdmitOutcome::Admitted);

    // ...and the same snapshot with the flag stripped is the engine nobody told
    let mut stripped = snap;
    stripped.account_ccy_set = false;
    let mut unknown = ExecutionEngine::from_snapshot(&stripped, RecordingClient::default());
    unknown.now_ms = T0;
    assert!(!unknown.account_ccy_known());
    assert_eq!(
        unknown.admit_symbol(&quoted(USDJPY, "JPY", R, T0)),
        AdmitOutcome::Refused("account-ccy-unset")
    );
}

/// `usd` and `USD` are one currency at every boundary a code enters: the admission's currency (against
/// the account's, and against another symbol's), and a fee or a balance a venue wrote in its own case.
#[test]
fn currency_codes_are_one_currency_whatever_their_case() {
    let mut e = engine();
    // the account's own currency, spelled any way: an ordinary symbol, no fx book
    let usd = SymbolAdmission { quote_ccy: Some(" usd ".into()), ..plain("GBPUSD") };
    assert_eq!(e.admit_symbol(&usd), AdmitOutcome::Admitted);
    assert!(!e.account.has_foreign_quote());

    // two spellings of one foreign currency are ONE rate row
    assert_eq!(e.admit_symbol(&quoted(USDJPY, " jpy ", R, T0)), AdmitOutcome::Admitted);
    assert_eq!(e.admit_symbol(&quoted(GBPJPY, "JPY", R, T0)), AdmitOutcome::Admitted);
    assert_eq!(e.account.quote_ccy_of(USDJPY), Some("JPY"), "kept in its canonical spelling");
    let (account_ccy, rows) = e.account.quote_rate_table(T0);
    assert_eq!(account_ccy, "USD");
    assert_eq!(rows.len(), 1, "one currency, one row: {rows:?}");
    // a refresh under a third spelling is a refresh, not a change of currency
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "Jpy", R, T0)), AdmitOutcome::Refreshed);

    // a fee a venue wrote as `usd` is the account's currency: booked, not left out as a third one
    let fee = |id: &'static str, amount: f64, asset: &str| FillEvent {
        commission: amount,
        commission_asset: asset.into(),
        ..fill_event(id, USDJPY, 1, 1_000.0, 150.0)
    };
    fold(&mut e, fee("f1", 1.5, "usd"));
    assert_eq!(e.account.balance, -1.5);
    assert_eq!(e.account.fees_unconverted(), 0);
    fold(&mut e, fee("f2", 150.0, "jpy"));
    assert_eq!(e.account.balance, -1.5 - 150.0 * R, "the quote currency's own fee, converted");
    fold(&mut e, fee("f3", 0.5, "eur"));
    assert_eq!(e.account.fees_unconverted(), 1, "a third currency is still left out");

    // a balance frame that names the account's currency in another case IS the ledger
    let frame = |pairs: &[(&str, f64)]| {
        Event::AccountState(AccountState {
            venue: VENUE.into(),
            balances: pairs.iter().map(|(a, q)| ((*a).to_string(), *q)).collect(),
            ts: T0,
            route_key: None,
        })
    };
    e.on_event(&frame(&[("JPY", 1_500_000.0), ("usd", 9_000.0)]), &mut Outbox::default());
    assert_eq!(e.account.balance, 9_000.0);
}
