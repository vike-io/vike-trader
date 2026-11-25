//! The refusal note: written by a refused start, read back word for word, removed by the next start.

use super::*;

#[test]
fn a_refused_start_leaves_its_reason_for_trade_health_and_the_next_start_removes_it() {
    let state = tempfile::tempdir().expect("a state dir");
    let dir = state.path();
    let file = dir.join(vike_paths::state_path::TRADEHUB_REFUSED_START_FILE);

    let _code = refused_start(Some(dir), "bad run profile row `r`: no [risk] table");
    let text = std::fs::read_to_string(&file).expect("the note is written");
    assert!(text.starts_with("refused at "), "{text}");
    assert!(text.contains("bad run profile row `r`: no [risk] table"), "{text}");

    clear_refusal_note(Some(dir));
    assert!(!file.exists(), "the next start removes the note");
    clear_refusal_note(Some(dir));
    clear_refusal_note(None);
}

#[test]
fn the_note_says_when_in_utc_then_the_refusal() {
    assert_eq!(note("why", 0), "refused at 1970-01-01T00:00:00Z\nwhy\n");
}
