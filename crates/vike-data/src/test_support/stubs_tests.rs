//! `hist_store_stubs!`'s own tests: both flavours over every required verb, and a partial list
//! composing with the verbs a double writes by hand.

use std::sync::atomic::{AtomicUsize, Ordering};

use vike_model::Bar;

use crate::store::hist::{DataError, HistStore, TsRange};

/// Every REQUIRED verb, called once with empty arguments: `(verb, Ok(rows or count) | Err)`.
///
/// The list is the trait's, spelled out here rather than derived, so a verb the macro forgot is a
/// compile error in the double (a missing trait item) and a verb this list forgot is a short
/// `len()` below.
fn call_every_required_verb(s: &dyn HistStore) -> Vec<(&'static str, Result<usize, DataError>)> {
    let r = TsRange::all();
    vec![
        ("load_bars", s.load_bars("v", "s", "1m", r).map(|x| x.len())),
        ("scan_quotes", s.scan_quotes("v", "s", r).map(|x| x.len())),
        ("scan_trades", s.scan_trades("v", "s", r).map(|x| x.len())),
        ("scan_book_updates", s.scan_book_updates("v", "s", r).map(|x| x.len())),
        ("scan_symbol_properties", s.scan_symbol_properties("v", "s", r).map(|x| x.len())),
        ("scan_equity", s.scan_equity("v", "s", r).map(|x| x.len())),
        ("scan_exec_fills", s.scan_exec_fills("v", "s").map(|x| x.len())),
        ("scan_exec_orders", s.scan_exec_orders("v", "s").map(|x| x.len())),
        ("append_bars", s.append_bars("v", "s", "1m", &[], None)),
        ("append_quotes", s.append_quotes("v", "s", &[], None)),
        ("append_trades", s.append_trades("v", "s", &[], None)),
        ("append_book_updates", s.append_book_updates("v", "s", &[], None)),
        ("append_symbol_properties", s.append_symbol_properties("v", "s", &[], None)),
        ("append_equity", s.append_equity("v", "s", &[], None)),
        ("append_exec_fills", s.append_exec_fills("v", "s", &[], None)),
        ("append_exec_orders", s.append_exec_orders("v", "s", &[], None)),
        ("resample_quotes_to_bars", s.resample_quotes_to_bars("v", "s", "1m", r, None)),
        ("resample_trades_to_bars", s.resample_trades_to_bars("v", "s", "1m", r, None)),
    ]
}

struct Inert;

impl HistStore for Inert {
    crate::hist_store_stubs!(inert: all);
}

fn planted(verb: &str) -> DataError {
    DataError::Query(format!("planted refusal ({verb})"))
}

struct Refusing;

impl HistStore for Refusing {
    crate::hist_store_stubs!(refuse(planted): all);
}

/// `inert: all` answers every one of the 18 required verbs `Ok`, with nothing in it.
#[test]
fn inert_answers_every_required_verb_ok_and_empty() {
    let answers = call_every_required_verb(&Inert);
    assert_eq!(answers.len(), 18, "the trait's required verbs");
    for (verb, answer) in answers {
        match answer {
            Ok(n) => assert_eq!(n, 0, "{verb}: an inert stub holds and writes nothing"),
            Err(e) => panic!("{verb}: an inert stub must not refuse, got {e}"),
        }
    }
}

/// `refuse(f): all` answers every required verb with `f`'s error, handed THAT verb's name.
#[test]
fn refuse_names_the_verb_in_every_error() {
    for (verb, answer) in call_every_required_verb(&Refusing) {
        match answer {
            Err(DataError::Query(m)) => assert_eq!(m, format!("planted refusal ({verb})")),
            other => panic!("{verb}: expected the planted refusal, got {other:?}"),
        }
    }
}

/// A double that writes `load_bars` by hand and stubs the rest by group and by name: the hand
/// verb answers its own rows and counts exactly, a DEFAULTED verb (`bar_edges`) still goes through
/// it, and the stubbed verbs stay inert.
struct OneBar {
    loads: AtomicUsize,
}

impl HistStore for OneBar {
    fn load_bars(&self, _: &str, _: &str, _: &str, _: TsRange) -> Result<Vec<Bar>, DataError> {
        self.loads.fetch_add(1, Ordering::Relaxed);
        Ok(vec![Bar {
            ts: 7,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }])
    }
    crate::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates);
    crate::hist_store_stubs!(refuse(planted): scan_symbol_properties, scan_equity, scan_exec_fills);
    crate::hist_store_stubs!(refuse(planted): scan_exec_orders,);
}

#[test]
fn a_partial_list_composes_with_the_verbs_written_by_hand() {
    let store = OneBar { loads: AtomicUsize::new(0) };
    let answers = call_every_required_verb(&store);
    assert_eq!(store.loads.load(Ordering::Relaxed), 1, "one call, counted once");

    let by_verb = |want: &str| answers.iter().find(|(v, _)| *v == want).map(|(_, a)| a);
    assert_eq!(by_verb("load_bars").unwrap().as_ref().ok(), Some(&1), "the hand verb's own row");
    assert_eq!(by_verb("append_bars").unwrap().as_ref().ok(), Some(&0), "`writes` is inert");
    assert_eq!(by_verb("scan_quotes").unwrap().as_ref().ok(), Some(&0), "named, inert");
    for verb in ["scan_symbol_properties", "scan_equity", "scan_exec_fills", "scan_exec_orders"] {
        let err = by_verb(verb).unwrap().as_ref().expect_err(verb);
        assert!(err.to_string().contains(&format!("({verb})")), "{verb}: {err}");
    }

    let edges = store.bar_edges("v", "s", "1m", TsRange::all()).unwrap();
    assert_eq!((edges.first_ts, edges.last_ts, edges.rows), (Some(7), Some(7), 1));
    assert_eq!(store.loads.load(Ordering::Relaxed), 2, "the trait default read through it");
}
