use super::*;

fn source(id: &str, received_ms: i64) -> TextSource {
    TextSource {
        text_id: id.to_string(),
        source: "Wire".to_string(),
        published_ms: received_ms - 1,
        received_ms,
        url: format!("https://a.test/{id}"),
    }
}

fn chunk(id: &str, received_ms: i64, text: &str, vector: &[f64]) -> Chunk {
    Chunk { of: source(id, received_ms), index: 0, text: text.to_string(), vector: vector.to_vec() }
}

fn ids(corpus: &Corpus, hits: &[Hit]) -> Vec<String> {
    hits.iter().map(|h| corpus.chunks()[h.chunk].of.text_id.clone()).collect()
}

#[test]
fn cosine_is_the_angle_and_refuses_what_it_cannot_compare() {
    assert_eq!(cosine(&[1.0, 0.0], &[2.0, 0.0]), Some(1.0));
    assert_eq!(cosine(&[1.0, 0.0], &[0.0, 3.0]), Some(0.0));
    assert_eq!(cosine(&[3.0, 4.0], &[4.0, 3.0]), Some(24.0 / 25.0));
    assert_eq!(cosine(&[1.0], &[1.0, 0.0]), None, "another length: another model");
    assert_eq!(cosine(&[], &[]), None);
    assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), None);
}

#[test]
fn reciprocal_rank_fusion_sums_one_over_k_plus_rank() {
    let hit = |chunk| Hit { chunk, score: 0.0 };
    let fused = reciprocal_rank_fusion(&[&[hit(0), hit(1)], &[hit(1), hit(2)]]);
    let expected = [(1, 1.0 / 62.0 + 1.0 / 61.0), (0, 1.0 / 61.0), (2, 1.0 / 62.0)];
    assert_eq!(fused.len(), 3);
    for (got, (chunk, score)) in fused.iter().zip(expected) {
        assert_eq!((got.chunk, got.score), (chunk, score));
    }
    // A tie keeps chunk order.
    let tie = reciprocal_rank_fusion(&[&[hit(5)], &[hit(3)]]);
    assert_eq!(tie.iter().map(|h| h.chunk).collect::<Vec<_>>(), [3, 5]);
}

#[test]
fn bm25_matches_the_formula_worked_by_hand() {
    let corpus = Corpus::new(vec![
        chunk("a", 1, "Apple banana", &[]),
        chunk("b", 1, "apple, APPLE cherry", &[]),
        chunk("c", 1, "cherry", &[]),
    ]);
    let ranking = corpus.keyword_ranking("apple apple", 1);
    // N = 3, df(apple) = 2, lengths 2, 3, 1, mean 2.
    let idf = libm::log(1.0 + (3.0 - 2.0 + 0.5) / (2.0 + 0.5));
    let score = |tf: f64, length: f64| {
        idf * tf * (BM25_K1 + 1.0) / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * length / 2.0))
    };
    assert_eq!(ids(&corpus, &ranking), ["b", "a"], "c shares no term");
    assert_eq!(ranking[0].score, score(2.0, 3.0));
    assert_eq!(ranking[1].score, score(1.0, 2.0));
    assert!((ranking[1].score - idf).abs() < 1e-15, "tf 1 at the mean length scores the idf");
    assert!(corpus.keyword_ranking("durian", 1).is_empty());
}

#[test]
fn a_chunk_received_after_the_decision_is_not_ranked_and_does_not_move_a_score() {
    let early =
        vec![chunk("a", 10, "rate cut", &[1.0, 0.0]), chunk("b", 20, "rate hike", &[0.0, 1.0])];
    let mut with_later = early.clone();
    with_later.push(chunk("c", 30, "rate rate rate", &[1.0, 0.0]));
    let (past, full) = (Corpus::new(early), Corpus::new(with_later));

    let at = |corpus: &Corpus| corpus.keyword_ranking("rate cut", 25);
    assert_eq!(at(&full), at(&past), "the later chunk changes no statistic of the earlier set");
    assert_eq!(ids(&full, &full.keyword_ranking("rate cut", 30))[0], "a");
    assert_eq!(full.keyword_ranking("rate", 30).len(), 3);

    assert_eq!(ids(&full, &full.vector_ranking(&[1.0, 0.0], 25)), ["a", "b"]);
    assert_eq!(ids(&full, &full.vector_ranking(&[1.0, 0.0], 30)), ["a", "c", "b"]);
    assert!(full.vector_ranking(&[1.0, 0.0], 5).is_empty(), "nothing was received yet");
}

#[test]
fn the_vector_ranking_skips_a_chunk_with_no_vector_or_another_length() {
    let corpus = Corpus::new(vec![
        chunk("none", 1, "x", &[]),
        chunk("short", 1, "x", &[1.0]),
        chunk("far", 1, "x", &[-1.0, 0.1]),
        chunk("near", 1, "x", &[0.9, 0.1]),
    ]);
    let ranking = corpus.vector_ranking(&[1.0, 0.0], 1);
    assert_eq!(ids(&corpus, &ranking), ["near", "far"]);
    assert!(ranking[0].score > 0.99 && ranking[1].score < -0.99);
}

#[test]
fn hybrid_retrieval_puts_a_chunk_both_rankings_find_first() {
    let corpus = Corpus::new(vec![
        chunk("words", 1, "bitcoin etf approved", &[0.0, 1.0]),
        chunk("both", 1, "bitcoin etf delayed", &[1.0, 0.1]),
        chunk("meaning", 1, "regulator postpones fund decision", &[1.0, 0.0]),
    ]);
    let hybrid = corpus.hybrid_ranking("bitcoin etf delayed", &[1.0, 0.0], 1);
    assert_eq!(ids(&corpus, &hybrid)[0], "both");
    assert_eq!(hybrid.len(), 3, "the vector ranking brings the chunk no keyword finds");
}

#[test]
fn a_text_is_cut_into_numbered_chunks_and_vectors_join_on_id_time_and_index() {
    let long = (0..200).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
    let mut chunks = chunks_of(&source("t", 7), "Title", "", &long);
    chunks.extend(chunks_of(&source("u", 8), "Short", "summary", ""));
    let indices: Vec<(String, u32)> =
        chunks.iter().map(|c| (c.of.text_id.clone(), c.index)).collect();
    assert_eq!(indices, [("t".into(), 0), ("t".into(), 1), ("u".into(), 0)]);
    assert!(chunks.iter().all(|c| c.vector.is_empty()));

    let attached = attach_vectors(
        &mut chunks,
        [
            ("t".to_string(), 7, 1, vec![1.0, 2.0]),
            ("u".to_string(), 8, 0, vec![3.0, 4.0]),
            ("u".to_string(), 9, 0, vec![9.0, 9.0]), // another revision's time: no chunk
            ("v".to_string(), 8, 0, vec![9.0, 9.0]), // another text: no chunk
        ],
    );
    assert_eq!(attached, 2);
    let vectors: Vec<Vec<f64>> = chunks.iter().map(|c| c.vector.clone()).collect();
    assert_eq!(vectors, [vec![], vec![1.0, 2.0], vec![3.0, 4.0]]);
}
