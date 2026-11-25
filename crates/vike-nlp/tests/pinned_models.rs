//! The REAL pinned weights, end to end: fetch both models from the hub at their pinned commits (about
//! 530 MB), verify every digest, and score known sentences. `#[ignore]`d because it downloads; run it
//! by hand on a lane, in release (a debug BERT forward is minutes):
//!
//! `cargo test -p vike-nlp --release --test pinned_models -- --ignored --nocapture`
//!
//! The assertions are ORDERINGS a correct model cannot miss (a rally reads positive, a plunge
//! negative; a paraphrase is nearer than an unrelated sentence), never a re-recorded number. The
//! printed values are the evidence a pin bump compares against (`crates/vike-nlp/src/pins.rs`'s
//! "Bumping a pin").

use vike_math::linear_algebra::dot;
use vike_nlp::embedding::EmbeddingModel;
use vike_nlp::pins::{EMBEDDING_MODEL, SENTIMENT_MODEL};
use vike_nlp::sentiment::SentimentModel;

/// An embedding widened to `f64` entry by entry, so its dot product is taken in `f64`.
fn widened(embedding: &[f32]) -> Vec<f64> {
    embedding.iter().map(|&x| f64::from(x)).collect()
}

#[test]
#[ignore = "downloads ~530 MB of pinned weights; run by hand in release"]
fn the_pinned_models_fetch_verify_and_score() {
    let scratch =
        vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike_nlp_pinned_models")
            .expect("scratch dir");
    let models = scratch.path();
    for model in [SENTIMENT_MODEL, EMBEDDING_MODEL] {
        vike_nlp::fetch::fetch(&model, models).unwrap_or_else(|e| panic!("{}: {e}", model.name));
        // A second fetch finds every file verified and downloads nothing.
        vike_nlp::fetch::fetch(&model, models).unwrap();
    }

    let sentiment = SentimentModel::load(&SENTIMENT_MODEL, models).unwrap();
    let rally = sentiment.score("Stocks rallied and the British pound gained.").unwrap();
    let plunge = sentiment.score("Shares plunged after the company reported a huge loss.").unwrap();
    let flat = sentiment.score("The company will publish its annual report on Tuesday.").unwrap();
    println!("rally  {rally:?} score {}", rally.score());
    println!("plunge {plunge:?} score {}", plunge.score());
    println!("flat   {flat:?} score {}", flat.score());
    assert!(rally.positive > rally.negative && rally.positive > rally.neutral, "{rally:?}");
    assert!(plunge.negative > plunge.positive && plunge.negative > plunge.neutral, "{plunge:?}");
    assert!(flat.neutral > flat.positive && flat.neutral > flat.negative, "{flat:?}");
    assert_eq!(rally, sentiment.score("Stocks rallied and the British pound gained.").unwrap());

    let encoder = EmbeddingModel::load(&EMBEDDING_MODEL, models).unwrap();
    assert_eq!(encoder.dimension(), 384);
    let anchor = encoder.embed("That is a happy person").unwrap();
    let near = encoder.embed("That is a very happy person").unwrap();
    let far = encoder.embed("Today is a sunny day").unwrap();
    let anchor_wide = widened(&anchor);
    let (near_similarity, far_similarity) =
        (dot(&anchor_wide, &widened(&near)), dot(&anchor_wide, &widened(&far)));
    println!("similarity: paraphrase {near_similarity}, unrelated {far_similarity}");
    assert!(near_similarity > 0.8 && far_similarity < 0.5, "{near_similarity} {far_similarity}");
    assert_eq!(anchor, encoder.embed("That is a happy person").unwrap());
}
