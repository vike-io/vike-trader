use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::{HistoryStoreMemory, RecordingSink};
use vike_marketdata::L2BookUpdateKind;
use vike_model::{AssetClass, Contract, ContractPredictionOutcome};

const TOKEN: &str = "71321045679252212594626385532706912750332728571942532289631379312455583992563";
const T: i64 = 1_794_124_800_000;

/// The venue's row of `token`: tick 0.01, five shares at least, a share step and a 250 ms hold.
fn row(token: &str) -> Instrument {
    let contract = Contract::PredictionOutcome(ContractPredictionOutcome {
        condition_id: "0xc".to_string(),
        outcome: "Up".to_string(),
        token_id: token.to_string(),
        resolution_ms: None,
        neg_risk: None,
    });
    Instrument::new("polymarket", token, "Up", "USDC", contract).with_properties(SymbolProperties {
        tick_size: 0.01,
        step_size: 0.01,
        min_qty: 5.0,
        taker_hold_ms: 250,
        ..Default::default()
    })
}

/// A store, a writer over a scripted lookup, and the count of lookups made.
fn writer(
    answer: impl Fn(&str) -> Result<Vec<Instrument>, String> + Send + Sync + 'static,
) -> (Arc<HistoryStoreMemory>, Arc<TokenFacts>, Arc<AtomicUsize>) {
    let store = Arc::new(HistoryStoreMemory::new());
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    let lookup: FactsLookup = Arc::new(move |token: &str| {
        count.fetch_add(1, Ordering::SeqCst);
        answer(token)
    });
    (Arc::clone(&store), TokenFacts::new(store, lookup), asked)
}

fn facts_at(store: &HistoryStoreMemory, ts: i64) -> Option<SymbolProperties> {
    store.properties_as_of("polymarket", TOKEN, ts).expect("the store answers")
}

/// **PM-2**: a token the recorder resolves gets ONE facts row in the store, beside its rows, stamped when it was
/// resolved: the venue's tick, minimum size, share step and hold, classed a prediction market. A later tick asks
/// the venue nothing more.
#[test]
fn a_resolved_token_gets_its_facts_row_once() {
    let (store, facts, asked) = writer(|t| Ok(vec![row(t)]));
    let mut resolver = RecordsTokenFacts::new(
        Box::new(vike_data::live::SymbolResolverFixed::new([TOKEN])),
        Arc::clone(&facts),
    );
    assert_eq!(resolver.desired(T).expect("resolves").len(), 1);
    assert_eq!(resolver.desired(T + 30_000).expect("resolves").len(), 1);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "one read per token");
    let got = facts_at(&store, T).expect("a facts row at the resolve");
    assert_eq!((got.tick_size, got.min_qty, got.step_size), (0.01, 5.0, 0.01));
    assert_eq!(got.taker_hold_ms, 250);
    assert_eq!(got.asset_class, Some(AssetClass::PredictionMarket));
    assert_eq!(facts_at(&store, T - 1), None, "nothing before the token was recorded");
}

/// A read that fails writes nothing and is asked again at the next tick; a token the venue lists no row of is the
/// same. The resolve itself is never failed by it.
#[test]
fn a_failed_read_writes_nothing_and_is_asked_again() {
    let (store, facts, asked) = writer(|_| Err("gatewayhub 127.0.0.1:1: refused".to_string()));
    let tokens = BTreeSet::from([TOKEN.to_string()]);
    facts.record(&tokens, T);
    facts.record(&tokens, T + 30_000);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert_eq!(facts_at(&store, T + 60_000), None);
    let (store, facts, _) = writer(|_| Ok(vec![row("another token")]));
    facts.record(&tokens, T);
    assert_eq!(facts_at(&store, T), None, "a row of another token is no row of this one");
}

/// **A `tick_size_change` updates the recorded step from that moment on**: the recorder's sink passes the book
/// update on and notes its new tick; the next tick writes a facts row with it, stamped at the change. Before the
/// change the old step answers, after it the new one; the same tick again writes nothing.
#[test]
fn a_recorded_tick_change_writes_the_new_step_from_its_moment() {
    let (store, facts, _) = writer(|t| Ok(vec![row(t)]));
    let tokens = BTreeSet::from([TOKEN.to_string()]);
    facts.record(&tokens, T);
    let inner = Arc::new(RecordingSink::default());
    let sink = TickWatch::new(Arc::clone(&inner) as Arc<dyn DataClientSink>, Arc::clone(&facts));
    let update = |ts: i64, tick: f64| L2BookUpdate {
        timestamp: ts,
        local_timestamp: ts,
        sequence: 1,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: tick,
        bids: Vec::new(),
        asks: Vec::new(),
        symbol: String::new(),
    };
    sink.book_update("polymarket", TOKEN, update(T + 10_000, 0.01));
    sink.book_update("polymarket", TOKEN, update(T + 20_000, 0.001));
    sink.book_update("polymarket", TOKEN, update(T + 25_000, 0.001));
    sink.book_update("binance", "BTCUSDT", update(T + 20_000, 0.5));
    assert_eq!(inner.book_updates().len(), 4, "every update reaches the store's sink");
    facts.record(&tokens, T + 30_000);
    assert_eq!(facts_at(&store, T + 19_999).map(|f| f.tick_size), Some(0.01));
    let after = facts_at(&store, T + 20_000).expect("the change's row");
    assert_eq!((after.tick_size, after.min_qty), (0.001, 5.0), "only the step moved");
    facts.record(&tokens, T + 60_000);
    let rows = store
        .scan_symbol_properties("polymarket", TOKEN, vike_data::TimestampRange::all())
        .expect("scan");
    assert_eq!(rows.len(), 2, "the resolve's row and the change's: {rows:?}");
}

/// A rolling family whose live windows are scripted, one set per tick.
struct ScriptedFamily(Vec<BTreeSet<String>>);

impl SymbolResolver for ScriptedFamily {
    fn group(&self) -> Option<&str> {
        Some("btc-updown-5m")
    }
    fn desired(&mut self, _now_ms: i64) -> Result<BTreeSet<String>, String> {
        Ok(self.0.remove(0))
    }
}

/// **`Request::LiveFamilyTokens` answers exactly the recorded family's current tokens**: each tick's resolved set,
/// with the outcome and window end its facts row states, replaces the family's entry wholesale, so a token that
/// rotated out is gone and the next window's are there. No read beyond the facts read is made for it.
#[test]
fn the_live_answer_is_exactly_the_familys_current_tokens() {
    // Token `<window><outcome>`: windows 1, 2 and 3 end at 300, 600 and 900 s; `u` is Up and `d` Down.
    let outcome_row = |token: &str| {
        let mut row = row(token);
        let end = token[..1].parse::<i64>().expect("window") * 300_000;
        let outcome = if token.ends_with('u') { "Up" } else { "Down" };
        row.contract = Contract::PredictionOutcome(ContractPredictionOutcome {
            condition_id: format!("0x{}", &token[..1]),
            outcome: outcome.to_string(),
            token_id: token.to_string(),
            resolution_ms: Some(end),
            neg_risk: None,
        });
        row
    };
    let store = Arc::new(HistoryStoreMemory::new());
    let held = crate::live_family_tokens::LiveFamilyTokensHeld::default();
    let lookup: FactsLookup = Arc::new(move |t: &str| Ok(vec![outcome_row(t)]));
    let facts = TokenFacts::publishing(store, lookup, None, held.clone());
    let set = |tokens: [&str; 4]| tokens.into_iter().map(str::to_string).collect::<BTreeSet<_>>();
    let mut family = RecordsTokenFacts::new(
        Box::new(ScriptedFamily(vec![
            set(["1u", "1d", "2u", "2d"]),
            set(["2u", "2d", "3u", "3d"]),
        ])),
        facts,
    );
    let answer = |held: &crate::live_family_tokens::LiveFamilyTokensHeld| {
        let vike_datahub_client::proto::Response::LiveFamilyTokens(tokens) =
            crate::live_family_tokens::live_family_tokens_verb(Some(held))
        else {
            panic!("answered")
        };
        tokens.into_iter().map(|t| (t.token, t.outcome, t.end_ms)).collect::<Vec<_>>()
    };
    let up = |token: &str, end: i64| (token.to_string(), "Up".to_string(), Some(end));
    let down = |token: &str, end: i64| (token.to_string(), "Down".to_string(), Some(end));

    family.desired(T).expect("resolves");
    assert_eq!(
        answer(&held),
        [down("1d", 300_000), up("1u", 300_000), down("2d", 600_000), up("2u", 600_000)]
    );
    family.desired(T + 300_000).expect("resolves");
    assert_eq!(
        answer(&held),
        [down("2d", 600_000), up("2u", 600_000), down("3d", 900_000), up("3u", 900_000)],
        "the rotated-out window is gone, the next one is there"
    );
}
