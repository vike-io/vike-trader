use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::HistoryStoreMemory;

const YES: &str = "71321045679252212594626385532706912750332728571942532289631379312455583992563";
const NO: &str = "11111111111111111111111111111111111111111111111111111111111111111111111111111";
const T: i64 = 1_794_124_800_000;

/// The market of the two tokens, as the gatewayhub hands it back, with `rules` as its wording.
fn market(rules: &str, received_ms: i64) -> TextRow {
    TextRow {
        source_id: "0xc".to_string(),
        subject: format!("{YES} {NO}"),
        title: "Will it rain?".to_string(),
        body: rules.to_string(),
        ..TextRow::new(TEXT_KIND_POLYMARKET_RULES, "Polymarket", received_ms)
    }
}

type Answer = Arc<Mutex<Result<Vec<TextRow>, String>>>;

/// A store, a writer over a scripted lookup whose answer a test can change, and the count of lookups made.
fn writer(store: Arc<HistoryStoreMemory>, answer: &Answer) -> (TokenRules, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let (count, answer) = (Arc::clone(&asked), Arc::clone(answer));
    let lookup: RulesLookup = Arc::new(move |_: &str| {
        count.fetch_add(1, Ordering::SeqCst);
        answer.lock().unwrap().clone()
    });
    (TokenRules::new(store, lookup), asked)
}

fn stored(store: &HistoryStoreMemory, at: i64) -> Vec<TextRow> {
    store
        .texts_as_of(
            TEXT_KIND_POLYMARKET_RULES,
            POLYMARKET_RULES_RECORDER_COLLECTOR,
            at,
            MS_PER_DAY * 30,
        )
        .expect("the store answers")
}

fn both() -> BTreeSet<String> {
    [YES, NO].iter().map(|t| (*t).to_string()).collect()
}

/// **W38**: a recorded market's rules are stored once, read once for both of its tokens, and not read again the
/// same day.
#[test]
fn a_recorded_market_is_stored_once_and_read_once_a_day() {
    let store = Arc::new(HistoryStoreMemory::new());
    let answer: Answer = Arc::new(Mutex::new(Ok(vec![market("Resolves Yes if it rains.", T)])));
    let (rules, asked) = writer(Arc::clone(&store), &answer);
    assert_eq!(rules.record(&both(), T), 1);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "one read covers both tokens of the market");
    assert_eq!(rules.record(&both(), T + 30_000), 0);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "not read again the same day");
    let rows = stored(&store, T + 60_000);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].amends.is_empty(), "a first sighting amends nothing");

    // The next day the market is read again; unchanged, nothing is written.
    assert_eq!(rules.record(&both(), T + MS_PER_DAY), 0);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
}

/// Changed rules are a new row of the same text, with `amends` naming the market.
#[test]
fn changed_rules_are_stored_again_with_amends() {
    let store = Arc::new(HistoryStoreMemory::new());
    let answer: Answer = Arc::new(Mutex::new(Ok(vec![market("Resolves Yes if it rains.", T)])));
    let (rules, _) = writer(Arc::clone(&store), &answer);
    rules.record(&both(), T);
    let next = T + MS_PER_DAY;
    *answer.lock().unwrap() =
        Ok(vec![market("Resolves Yes if it rains. Clarification: drizzle counts.", next)]);
    assert_eq!(rules.record(&both(), next), 1);
    let rows = stored(&store, next + 1);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].text_id(), rows[1].text_id(), "one market, one text");
    assert_eq!(rows[1].amends, "0xc", "the change names the market it amends");
    assert_eq!(rows[1].received_ms, next);
}

/// A restart learns from the store what it stored: the same rules are not stored again.
#[test]
fn a_restart_stores_nothing_again() {
    let store = Arc::new(HistoryStoreMemory::new());
    let answer: Answer = Arc::new(Mutex::new(Ok(vec![market("Resolves Yes if it rains.", T)])));
    let (first, _) = writer(Arc::clone(&store), &answer);
    first.record(&both(), T);
    let (second, asked) = writer(Arc::clone(&store), &answer);
    assert_eq!(second.record(&both(), T + 60_000), 0);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "read again, written not");
    assert_eq!(stored(&store, T + 120_000).len(), 1);
}

/// A failed read writes nothing and is asked again at the next tick; a token no open market lists writes
/// nothing; a row of another kind is never stored.
#[test]
fn a_failed_read_is_asked_again_and_nothing_wrong_is_stored() {
    let store = Arc::new(HistoryStoreMemory::new());
    let answer: Answer = Arc::new(Mutex::new(Err("gatewayhub down".to_string())));
    let (rules, asked) = writer(Arc::clone(&store), &answer);
    let one: BTreeSet<String> = [YES.to_string()].into();
    assert_eq!(rules.record(&one, T), 0);
    assert_eq!(rules.record(&one, T + 1), 0);
    assert_eq!(asked.load(Ordering::SeqCst), 2, "a failed read is asked again");

    *answer.lock().unwrap() = Ok(Vec::new());
    assert_eq!(rules.record(&one, T + 2), 0);
    *answer.lock().unwrap() = Ok(vec![TextRow::new("news", "x", T)]);
    assert_eq!(rules.record(&one, T + MS_PER_DAY), 0);
    assert!(stored(&store, T + 2 * MS_PER_DAY).is_empty());
}

/// L6: a tick makes at most `RULES_READS_PER_TICK` reads, so a wide family (or a slow Gamma) cannot stall the
/// recorder's subscription refresh by one read bound per market; the tokens left over are read by the next ticks,
/// and none is read twice the same day.
#[test]
fn a_tick_reads_at_most_its_cap_and_the_rest_wait_for_the_next_ticks() {
    let store = Arc::new(HistoryStoreMemory::new());
    let answer: Answer = Arc::new(Mutex::new(Ok(Vec::new())));
    let (rules, asked) = writer(store, &answer);
    let tokens: BTreeSet<String> = (0..10).map(|k| format!("token-{k}")).collect();
    let mut seen = Vec::new();
    for tick in 0..4 {
        rules.record(&tokens, T + tick);
        seen.push(asked.load(Ordering::SeqCst));
    }
    assert_eq!(RULES_READS_PER_TICK, 4);
    assert_eq!(seen, [4, 8, 10, 10], "four, four, the last two, then nothing the same day");
}
