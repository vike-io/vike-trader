use super::*;
use crate::store::store_kind::StoreKindId;

/// **THE PIN: the four spellings, byte for byte.** They are wire and stored strings (a
/// `vike_model::StreamStatus` disclosure names its lane by one), and every producer and consumer in
/// the tree now reaches them through this table, so this is the ONE place a literal is allowed to
/// be compared with a variant. Changing a word here changes what a deployed feed tells a deployed
/// sink.
///
/// The ORDER is pinned too: it is the order the datahub broker's lanes and the recorder's `Stream`
/// have always had, and the derived `Ord` of both keyed their maps.
const SPELLINGS: [(StreamLabel, &str); 4] = [
    (StreamLabel::Quotes, "quotes"),
    (StreamLabel::Trades, "trades"),
    (StreamLabel::Book, "book"),
    (StreamLabel::Depth, "depth"),
];

#[test]
fn the_four_spellings_and_their_order_are_pinned() {
    assert_eq!(StreamLabel::ALL.len(), SPELLINGS.len(), "ALL holds exactly the pinned labels");
    for (i, (label, word)) in SPELLINGS.iter().enumerate() {
        assert_eq!(StreamLabel::ALL[i], *label, "ALL is in declaration order at {i}");
        assert_eq!(label.as_str(), *word, "{label:?}");
    }
    let mut by_ord = StreamLabel::ALL.to_vec();
    by_ord.sort();
    assert_eq!(by_ord, StreamLabel::ALL, "the derived Ord is the declaration order");
}

/// Compile-time tie for the table above: a variant added to `stream_labels!` fails to build THIS
/// match until the author decides what the pinned table says about it.
fn pinned_word(label: StreamLabel) -> &'static str {
    match label {
        StreamLabel::Quotes => "quotes",
        StreamLabel::Trades => "trades",
        StreamLabel::Book => "book",
        StreamLabel::Depth => "depth",
    }
}

#[test]
fn every_label_has_a_pinned_word_and_round_trips() {
    for &label in StreamLabel::ALL {
        assert_eq!(label.as_str(), pinned_word(label), "{label:?}");
        assert_eq!(StreamLabel::from_label(label.as_str()), Some(label), "{label:?}");
    }
}

#[test]
fn the_spellings_are_distinct_lowercase_ascii_words() {
    let mut seen = std::collections::BTreeSet::new();
    for label in StreamLabel::ALL {
        let word = label.as_str();
        assert!(
            !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()),
            "`{word}` must be a lowercase ASCII word"
        );
        assert!(seen.insert(word), "`{word}` is spelled by two labels");
    }
}

/// A word that is not exactly a label is `None`, never a near match: a bar interval, the HFT
/// link label, the store's singular `kind=` spellings, and case and whitespace variants.
#[test]
fn nothing_but_the_four_words_parses() {
    for not_a_label in [
        "", "1m", "5m", "1h", "1d", "ticks", "quote", "trade", "Quotes", "TRADES", "Book", "Depth",
        " depth", "depth ", "book\n", "l2", "bars",
    ] {
        assert_eq!(StreamLabel::from_label(not_a_label), None, "{not_a_label:?}");
    }
}

/// ⚠ The label is the SUBSCRIPTION verb, the store kind is the PERSISTED series: `quotes`/`trades`
/// are plural here and singular there, `book`/`depth` coincide. The coincidence is what hides the
/// near-miss, so both halves are held.
#[test]
fn a_label_is_not_a_store_kind_spelling() {
    assert_eq!(StoreKindId::Book.as_str(), StreamLabel::Book.as_str());
    assert_eq!(StoreKindId::Depth.as_str(), StreamLabel::Depth.as_str());
    assert_ne!(StoreKindId::Quote.as_str(), StreamLabel::Quotes.as_str());
    assert_ne!(StoreKindId::Trade.as_str(), StreamLabel::Trades.as_str());
}

/// The label ↔ `LiveVerb` maps are inverse on the four tick verbs, and `Bars` — whose stream label
/// is an interval — has none.
#[test]
fn the_live_verb_maps_are_inverse_and_bars_has_no_label() {
    for &label in StreamLabel::ALL {
        assert_eq!(StreamLabel::from_live_verb(label.live_verb()), Some(label), "{label:?}");
    }
    assert_eq!(StreamLabel::from_live_verb(LiveVerb::Bars), None);

    // Compile-time tie: a new `LiveVerb` fails to build this match until it is decided whether it
    // is a tick lane (and so has a label) or not.
    fn is_tick_verb(verb: LiveVerb) -> bool {
        match verb {
            LiveVerb::Bars => false,
            LiveVerb::Quotes | LiveVerb::Trades | LiveVerb::Book | LiveVerb::Depth => true,
        }
    }
    for verb in
        [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth]
    {
        assert_eq!(StreamLabel::from_live_verb(verb).is_some(), is_tick_verb(verb), "{verb:?}");
    }
}
