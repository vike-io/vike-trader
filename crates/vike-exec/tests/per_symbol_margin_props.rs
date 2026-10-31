//! Property suite for the per-symbol initial-margin rate an admission can carry
//! (`SymbolAdmission::im_requirement`, written by `ExecutionEngine::admit_symbol` into
//! `RiskLimits::im_by_symbol` and read by `RiskLimits::im_for` ahead of the engine-wide rate).
//!
//! The laws:
//!
//! 8. THE ADMISSION IS THE COMPLETE STATEMENT: after any sequence of admissions, `im_for(symbol)` is
//!    the rate the LAST admission of an admitted symbol stated, else the engine-wide rate (a repeat
//!    that states none REMOVES the row); a value outside `(0, 1]` (NaN, infinities, zero, negatives,
//!    above one) is IGNORED, which is stating none, and refuses nothing; the MOUNTED symbol is the
//!    exception that keeps its row through a `None` (it may be the operator's); a valid rate never
//!    survives a LATER refusal of the same admission (grid, cap, currency).
//! 9. SNAPSHOT: `from_snapshot` (and the JSON the journal writes) preserves every `im_for`.
//! 10. MONOTONE GATE: the `insufficient-margin` verdict is monotone in the symbol's own rate: a
//!     higher rate never admits an order a lower rate refused (same order, same equity, same book).
//! 11. NO FIELD, NO TRACE: admissions that state no rate leave `im_by_symbol` empty and the engine
//!     snapshot equal to that of an engine that never saw the field (admitted through JSON without
//!     the key, which is what every pre-feature node sends).
//!
//! FAILURE POLICY: a counterexample is a REAL finding about the engine; commit the seed beside this
//! file as `per_symbol_margin_props.proptest-regressions` and never adjust a bound to pass
//! (decision 0021). Own test binary ON PURPOSE (`crates/vike-exec/CLAUDE.md`). No wall clock is read.

use proptest::prelude::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AdmitOutcome, BalanceMode, EngineSnapshot, ExecutionEngine, MAX_EXTRA_SYMBOLS, Outbox,
    RiskGate, SymbolAdmission, state_hash,
};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{ContractValue, OrderRequest, OrderType, RiskLimits, SymbolGrid};

const VENUE: &str = "sim";
const ACCT: &str = "USD";
const MOUNTED: &str = "MNT";
/// Index 0 is the mounted symbol; the rest are admitted on demand.
const NAMES: [&str; 5] = [MOUNTED, "S1", "S2", "S3", "S4"];
const NOW: i64 = 1_700_000_000_000;

fn engine(global: Option<f64>) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.01),
        lot_size: Some(1.0),
        im_requirement: global,
        ..RiskLimits::new()
    };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    );
    e.quote_asset = ACCT.to_string();
    e.now_ms = NOW;
    e
}

fn admission(symbol: &str, im: Option<f64>) -> SymbolAdmission {
    SymbolAdmission {
        route_key: VENUE.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(100.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: im,
        release: false,
        margin_mode: None,
    }
}

fn is_valid(x: f64) -> bool {
    x.is_finite() && x > 0.0 && x <= 1.0
}

/// A rate inside `(0, 1]`, edges included: exactly one, the smallest normal, the smallest subnormal.
fn valid_im() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => 1e-6f64..=1.0,
        2 => 0.01f64..0.5,
        1 => Just(1.0f64),
        1 => Just(f64::MIN_POSITIVE),
        1 => Just(f64::from_bits(1)),
    ]
}

/// Everything outside `(0, 1]`: not a number, either infinity, both zeros, negatives, and above one
/// down to the next double after it.
fn invalid_im() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(0.0f64),
        Just(-0.0f64),
        Just(1.0 + f64::EPSILON),
        Just(f64::MAX),
        Just(f64::MIN),
        Just(-f64::MIN_POSITIVE),
        -1e6f64..-1e-300,
        1.0001f64..1e6,
    ]
}

fn any_im() -> impl Strategy<Value = Option<f64>> {
    prop_oneof![
        2 => Just(None),
        5 => valid_im().prop_map(Some),
        2 => invalid_im().prop_map(Some),
    ]
}

type Ops = Vec<(usize, Option<f64>)>;

fn admissions() -> impl Strategy<Value = Ops> {
    prop::collection::vec((0usize..NAMES.len(), any_im()), 0..40)
}

proptest! {
    /// LAW 8a. `im_for` is the rate the last admission of the symbol stated, else the engine-wide one,
    /// after EVERY step; a repeat admission that states none (or a figure outside `(0, 1]`, ignored)
    /// removes an admitted symbol's row and leaves the mounted symbol's; no figure, however bad,
    /// refuses an admission; the mounted symbol takes a stated rate too (and stays `Mounted`).
    #[test]
    fn a_symbols_own_margin_rate_is_the_last_one_stated_else_the_engine_wide_one(
        global in prop::option::of(valid_im()),
        ops in admissions(),
    ) {
        let mut e = engine(global);
        let mut want: [Option<f64>; NAMES.len()] = [None; NAMES.len()];
        for (sym, im) in &ops {
            let out = e.admit_symbol(&admission(NAMES[*sym], *im));
            prop_assert!(
                matches!(out, AdmitOutcome::Admitted | AdmitOutcome::Refreshed | AdmitOutcome::Mounted),
                "{out:?}"
            );
            prop_assert_eq!(out == AdmitOutcome::Mounted, *sym == 0);
            let stated = im.filter(|x| is_valid(*x));
            if *sym == 0 {
                // the mounted symbol: a valid figure writes, anything else writes nothing
                if stated.is_some() {
                    want[0] = stated;
                }
            } else {
                // an admitted symbol: the admission is the whole statement of its row
                want[*sym] = stated;
            }
            for (i, name) in NAMES.iter().enumerate() {
                prop_assert_eq!(
                    e.gate.limits.im_for(name).map(f64::to_bits),
                    want[i].or(global).map(f64::to_bits),
                    "{} after ({}, {:?})", name, sym, im
                );
            }
        }
        prop_assert_eq!(e.gate.limits.im_by_symbol.len(), want.iter().flatten().count());
        prop_assert_eq!(e.gate.limits.im_for("NEVER-ADMITTED"), global);
    }
}

#[derive(Debug, Clone, Copy)]
enum Later {
    GridIncomplete,
    SymbolCap,
    NoQuoteRate,
    QuoteCcyMissing,
}

proptest! {
    /// LAW 8b. A valid rate rides an admission that is refused for ANOTHER reason (an incomplete
    /// grid, a full symbol table, a foreign currency with no rate, a rate with no currency): nothing
    /// is written, the rate included.
    #[test]
    fn a_valid_margin_rate_does_not_survive_a_later_refusal_of_its_admission(
        im in valid_im(),
        global in prop::option::of(valid_im()),
        which in prop::sample::select(vec![
            Later::GridIncomplete,
            Later::SymbolCap,
            Later::NoQuoteRate,
            Later::QuoteCcyMissing,
        ]),
    ) {
        let mut e = engine(global);
        if matches!(which, Later::SymbolCap) {
            for i in 0..MAX_EXTRA_SYMBOLS {
                let filled = e.admit_symbol(&admission(&format!("C{i}"), None));
                prop_assert_eq!(filled, AdmitOutcome::Admitted);
            }
        }
        let snap = e.snapshot_state();
        let (adm, word) = match which {
            Later::GridIncomplete => (
                SymbolAdmission {
                    grid: SymbolGrid { tick_size: None, lot_size: Some(1.0), ..SymbolGrid::default() },
                    ..admission("NEW", Some(im))
                },
                "grid-incomplete",
            ),
            Later::SymbolCap => (admission("NEW", Some(im)), "symbol-cap"),
            Later::NoQuoteRate => (
                SymbolAdmission { quote_ccy: Some("JPY".into()), ..admission("NEW", Some(im)) },
                "no-quote-rate",
            ),
            Later::QuoteCcyMissing => (
                SymbolAdmission { quote_rate: Some(1.0), ..admission("NEW", Some(im)) },
                "quote-ccy-missing",
            ),
        };
        prop_assert_eq!(e.admit_symbol(&adm), AdmitOutcome::Refused(word));
        prop_assert_eq!(e.snapshot_state(), snap);
        prop_assert_eq!(e.gate.limits.im_for("NEW"), global);
    }

    /// LAW 9. The engine rebuilt from its snapshot (directly, and through the JSON the journal
    /// writes) margins every symbol as the live one did, and snapshots equal.
    #[test]
    fn a_snapshot_round_trip_preserves_every_margin_rate(
        global in prop::option::of(valid_im()),
        ops in admissions(),
    ) {
        let mut e = engine(global);
        for (sym, im) in &ops {
            e.admit_symbol(&admission(NAMES[*sym], *im));
        }
        let snap = e.snapshot_state();
        let json = serde_json::to_string(&snap).expect("a snapshot serializes");
        let parsed: EngineSnapshot = serde_json::from_str(&json).expect("and reads back");
        prop_assert_eq!(&parsed, &snap);
        for restored in [
            ExecutionEngine::from_snapshot(&snap, RecordingClient::default()),
            ExecutionEngine::from_snapshot(&parsed, RecordingClient::default()),
        ] {
            for name in NAMES.iter().copied().chain(["NEVER-ADMITTED"]) {
                prop_assert_eq!(
                    restored.gate.limits.im_for(name).map(f64::to_bits),
                    e.gate.limits.im_for(name).map(f64::to_bits),
                    "{}", name
                );
            }
            prop_assert_eq!(restored.snapshot_state(), snap.clone());
        }
    }
}

// ---- law 10: the gate is monotone in the symbol's own rate -------------------------------------

fn order(coid: &str, symbol: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: OrderType::Limit,
        price: Some(px),
        ts: 1,
        ..Default::default()
    }
}

fn fill(id: usize, symbol: &str, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("T", id),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// Everything about one margin-gated BUY of `qty` at `px` on S1 except S1's own rate: the engine-wide
/// rate (0.5), the equity, a long book in S1 (`held1`, marked at 100) and another in S2 at its own
/// 0.3.
struct Book {
    equity: f64,
    held1: f64,
    held2: f64,
    qty: f64,
    px: f64,
}

impl Book {
    /// The gate's denial reason with S1 margined at `own_rate`, or `None` when the order reached the
    /// client.
    fn verdict(&self, own_rate: f64) -> Option<String> {
        let mut e = engine(Some(0.5));
        e.equity_seed = self.equity;
        e.admit_symbol(&admission("S1", Some(own_rate)));
        e.admit_symbol(&admission("S2", Some(0.3)));
        if self.held1 > 0.0 {
            e.account.apply_fill(&fill(1, "S1", self.held1));
        }
        if self.held2 > 0.0 {
            e.account.apply_fill(&fill(2, "S2", self.held2));
        }
        let mut outbox = Outbox::default();
        let sent = e.client.submissions.len();
        e.submit_order(&order("o1", "S1", 1, self.qty, self.px), 1_000, &mut outbox);
        let denied: Vec<String> = std::mem::take(&mut outbox.0)
            .into_iter()
            .filter_map(|ev| match ev {
                Event::OrderDenied(d) => Some(d.reason.to_string()),
                _ => None,
            })
            .collect();
        match denied.as_slice() {
            [] => {
                assert_eq!(e.client.submissions.len(), sent + 1, "an undenied order is sent");
                None
            }
            [one] => Some(one.clone()),
            many => panic!("one order, several denials: {many:?}"),
        }
    }
}

proptest! {
    /// LAW 10. For the same order, equity and book, raising the symbol's own rate never turns a
    /// refusal into an admission. The only denial the book can meet is `insufficient-margin`.
    /// Same-direction orders only: a reversal frees margin by a credit that grows with the rate too,
    /// and the sum of the two roundings is not monotone in the last bit.
    #[test]
    fn the_insufficient_margin_verdict_is_monotone_in_the_symbols_own_rate(
        equity in 100.0f64..50_000.0,
        held1 in 0u8..6,
        held2 in 0u8..6,
        qty in 1u8..25,
        px in 50.0f64..200.0,
        rates in (1e-3f64..=1.0, 1e-3f64..=1.0),
    ) {
        let book = Book {
            equity,
            held1: f64::from(held1),
            held2: f64::from(held2),
            qty: f64::from(qty),
            px,
        };
        let (lo, hi) = if rates.0 <= rates.1 { rates } else { (rates.1, rates.0) };
        let (at_lo, at_hi) = (book.verdict(lo), book.verdict(hi));
        for reason in [&at_lo, &at_hi].into_iter().flatten() {
            prop_assert_eq!(reason.as_str(), "insufficient-margin");
        }
        prop_assert!(
            at_lo.is_none() || at_hi.is_some(),
            "refused at {lo} but admitted at {hi}: {at_lo:?} then {at_hi:?}"
        );
    }
}

/// The monotone property can bite: at the edge a rate of 0.5 fits an order that a rate of 1.0 does not.
#[test]
fn a_higher_own_rate_does_refuse_what_a_lower_one_admits() {
    let book = Book { equity: 1_000.0, held1: 0.0, held2: 0.0, qty: 11.0, px: 100.0 };
    assert_eq!(book.verdict(0.5), None, "550 of margin fits 1,000 of equity");
    assert_eq!(book.verdict(1.0).as_deref(), Some("insufficient-margin"), "1,100 does not");
}

// ---- law 11: no field, no trace ---------------------------------------------------------------

#[derive(Debug, Clone)]
enum Say {
    /// Admit the symbol (index into `NAMES`).
    Plain(usize),
    /// Admit S3 quoted in JPY at `r0 * factor`.
    Foreign(f64),
}

fn say() -> impl Strategy<Value = Say> {
    prop_oneof![
        3 => (0usize..NAMES.len()).prop_map(Say::Plain),
        1 => (0.85f64..1.17).prop_map(Say::Foreign),
    ]
}

fn say_it(s: &Say, now: i64) -> SymbolAdmission {
    match s {
        Say::Plain(i) if NAMES[*i] == "S4" => SymbolAdmission {
            quote_ccy: Some(ACCT.into()),
            contract_value: Some(ContractValue::linear(10.0)),
            ..admission("S4", None)
        },
        Say::Plain(i) => admission(NAMES[*i], None),
        Say::Foreign(f) => SymbolAdmission {
            contract_value: Some(ContractValue::linear(1_000.0)),
            quote_ccy: Some("JPY".into()),
            quote_rate: Some(f / 150.0),
            rate_as_of_ms: Some(now),
            ..admission("S3", None)
        },
    }
}

proptest! {
    /// LAW 11. Admissions that state no margin rate leave `im_by_symbol` empty, and an engine fed them
    /// as structs snapshots (and hashes) equal to one fed the same admissions as JSON WITHOUT the
    /// key, the form every pre-feature node sends: the field leaves no trace.
    #[test]
    fn admissions_that_state_no_margin_rate_leave_the_engine_as_it_was(
        global in prop::option::of(valid_im()),
        says in prop::collection::vec((say(), 0i64..30_000), 0..30),
    ) {
        let (mut as_struct, mut as_json) = (engine(global), engine(global));
        for (s, dt) in &says {
            as_struct.now_ms += dt;
            as_json.now_ms += dt;
            let adm = say_it(s, as_struct.now_ms);
            let json = serde_json::to_string(&adm).expect("an admission serializes");
            prop_assert!(!json.contains("im_requirement"), "absent when None: {json}");
            let read: SymbolAdmission = serde_json::from_str(&json).expect("and reads back");
            prop_assert_eq!(&read, &adm);
            prop_assert_eq!(as_struct.admit_symbol(&adm), as_json.admit_symbol(&read));
        }
        prop_assert!(as_struct.gate.limits.im_by_symbol.is_empty());
        prop_assert!(as_json.gate.limits.im_by_symbol.is_empty());
        let (a, b) = (as_struct.snapshot_state(), as_json.snapshot_state());
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(state_hash(&[a]), state_hash(&[b]));
    }
}

/// The wire an older node sends (no `im_requirement` key) decodes to a `None`: the literal below is
/// the pre-feature `AdmitSymbol` payload byte for byte.
#[test]
fn a_pre_feature_admission_decodes_with_no_margin_rate() {
    let wire = r#"{"route_key":"sim","symbol":"S1","grid":{"tick_size":0.01,"lot_size":1.0},"initial_mark":100.0}"#;
    let read: SymbolAdmission = serde_json::from_str(wire).expect("an older node's payload");
    assert_eq!(read, admission("S1", None));
    assert_eq!(serde_json::to_string(&read).expect("serializes"), wire);
}
