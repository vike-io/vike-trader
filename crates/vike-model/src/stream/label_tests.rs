use super::*;

/// **THE PIN: the four spellings, byte for byte.** They are wire and stored strings (a
/// `crate::StreamStatus` disclosure names its lane by one), and every producer and consumer in
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
        assert_eq!(label.word(), *word, "{label:?}");
    }
    let mut by_ord = StreamLabel::ALL.to_vec();
    by_ord.sort();
    assert_eq!(by_ord, StreamLabel::ALL, "the derived Ord is the declaration order");
}

/// **The serde TAG is the variant name, not the word**: the wire lanes carry these, and a
/// `rename_all = "lowercase"` would silently swap them for the words.
#[test]
fn the_serde_tags_are_the_variant_names() {
    for label in StreamLabel::ALL {
        let tag = serde_json::to_string(&label).expect("a unit variant encodes");
        assert_eq!(tag, format!("\"{label:?}\""), "{label:?}");
        assert_eq!(serde_json::from_str::<StreamLabel>(&tag).expect("decodes"), label);
    }
    assert!(serde_json::from_str::<StreamLabel>("\"quotes\"").is_err(), "the word is no tag");
}

#[test]
fn the_spellings_are_distinct_lowercase_ascii_words() {
    let mut seen = std::collections::BTreeSet::new();
    for label in StreamLabel::ALL {
        let word = label.word();
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
        assert_eq!(StreamLabel::from_word(not_a_label), None, "{not_a_label:?}");
    }
}

/// **The tape-hole word stays no label**: the recorder's `match` over labels and the strategy lane's
/// admission both read it as "not mine" only while this holds.
#[test]
fn the_tape_hole_word_is_no_label_and_no_bar_interval() {
    assert_eq!(StreamLabel::from_word(TAPE_HOLE_WORD), None);
    assert!(
        !TAPE_HOLE_WORD.starts_with(|c: char| c.is_ascii_digit()),
        "an interval opens on a digit"
    );
}
