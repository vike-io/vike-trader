use super::*;

/// `count` words `<tag>0 <tag>1 …`.
fn words(tag: &str, count: usize) -> String {
    (0..count).map(|i| format!("{tag}{i}")).collect::<Vec<_>>().join(" ")
}

#[test]
fn a_text_that_fits_is_one_chunk_its_whole_text_verbatim() {
    assert_eq!(whole_text("Stocks rallied", "", ""), "Stocks rallied");
    assert_eq!(
        whole_text("Stocks rallied", "The pound gained.", ""),
        "Stocks rallied\nThe pound gained."
    );
    assert_eq!(whole_text("", "", "Body only."), "Body only.");
    assert_eq!(
        chunk_texts("Stocks rallied", "The pound gained.", ""),
        ["Stocks rallied\nThe pound gained."]
    );
    assert_eq!(chunk_texts("Headline only", "", ""), ["Headline only"]);
    // Exactly the budget still fits, blank lines and all, kept verbatim.
    let summary = format!("{}\n\n{}", words("a", 64), words("b", 64));
    assert_eq!(chunk_texts("T", &summary, ""), [format!("T\n{summary}")]);
}

#[test]
fn a_text_with_no_word_has_no_chunk() {
    assert!(chunk_texts("", "", "").is_empty());
    assert!(chunk_texts("", "  \n ", "\n").is_empty());
}

#[test]
fn paragraphs_pack_greedily_under_the_title_and_never_split_when_they_fit() {
    let body = [words("p", 50), words("q", 50), words("r", 50), words("s", 50)].join("\n\n");
    let chunks = chunk_texts("Item 1A. Risk Factors", "", &body);
    assert_eq!(
        chunks,
        [
            format!("Item 1A. Risk Factors\n{}\n\n{}", words("p", 50), words("q", 50)),
            format!("Item 1A. Risk Factors\n{}\n\n{}", words("r", 50), words("s", 50)),
        ]
    );
}

#[test]
fn a_paragraph_longer_than_the_budget_is_cut_into_runs_of_words() {
    let long = words("w", 300);
    let body = format!("{}\n\n{long}\n\n{}", words("a", 10), words("z", 5));
    let chunks = chunk_texts("Item 7", "", &body);
    let all: Vec<&str> = long.split(' ').collect();
    assert_eq!(
        chunks,
        [
            format!("Item 7\n{}", words("a", 10)),
            format!("Item 7\n{}", all[..128].join(" ")),
            format!("Item 7\n{}", all[128..256].join(" ")),
            format!("Item 7\n{}", all[256..].join(" ")),
            format!("Item 7\n{}", words("z", 5)),
        ]
    );
}

#[test]
fn the_summary_comes_before_the_body_and_blank_lines_with_spaces_or_carriage_returns_separate() {
    let summary = words("s", 110);
    let body = format!("{}\r\n \r\n{}\r\n", words("b", 20), words("c", 20));
    let chunks = chunk_texts("", &summary, &body);
    assert_eq!(chunks, [summary.clone(), format!("{}\n\n{}", words("b", 20), words("c", 20))]);
    assert!(chunks.iter().all(|c| c.split_whitespace().count() <= CHUNK_MAX_WORDS));
}
