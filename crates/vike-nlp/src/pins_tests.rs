use super::*;

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn every_pin_is_a_commit_and_every_file_a_digest() {
    for model in PINNED_MODELS {
        assert!(is_lower_hex(model.revision, 40), "{}: a revision is a 40-hex commit", model.name);
        for name in [CONFIG_FILE, VOCABULARY_FILE, WEIGHTS_FILE] {
            let file = model.files.iter().find(|f| f.path == name);
            let file = file.unwrap_or_else(|| panic!("{} pins no {name}", model.name));
            assert!(is_lower_hex(file.sha256, 64), "{}/{name}: sha256 is 64 hex", model.name);
            assert!(file.size > 0, "{}/{name}: a size", model.name);
        }
        assert!(model.max_tokens > 2, "{}: room for [CLS] and [SEP]", model.name);
        assert_eq!(pinned(model.name), Some(&model), "names are unique");
    }
    assert_eq!(pinned("no-such-model"), None);
}

#[test]
fn a_model_lives_under_its_name_and_revision_and_is_fetched_at_that_commit() {
    let dir = SENTIMENT_MODEL.folder_in(std::path::Path::new("models"));
    assert_eq!(dir, std::path::Path::new("models").join("finbert").join(SENTIMENT_MODEL.revision));
    assert_eq!(
        SENTIMENT_MODEL.file_url(&SENTIMENT_MODEL.files[0]),
        "https://huggingface.co/ProsusAI/finbert/resolve/0bc2ac10dda6929bcb1f77ccc43c7ec5c424f031/config.json"
    );
    assert_eq!(EMBEDDING_MODEL.label(), format!("all-minilm-l6-v2@{}", EMBEDDING_MODEL.revision));
}
