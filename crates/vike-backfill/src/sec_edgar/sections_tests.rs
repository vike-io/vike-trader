use super::{WHOLE_DOCUMENT, split_sections};

/// A 10-K's shape: a cover, a table of contents naming every item, then the items themselves.
const TEN_K: &str = "EXAMPLE CORP\nFORM 10-K\nTABLE OF CONTENTS\nPART I\nItem 1. Business 1\n\
Item 1A. Risk Factors 5\nPART II\nItem 7. Management's Discussion and Analysis 20\n\
PART I\nItem 1. Business\nWe make things.\nWe sell them.\nItem 1A. Risk Factors\n\
Our business is risky in many ways that a reader should know.\nPART II\n\
Item 7. Management's Discussion and Analysis\nRevenue grew because customers bought more.";

#[test]
fn a_10k_keeps_the_body_headings_and_skips_the_contents() {
    let sections = split_sections("10-K", TEN_K);
    let keys: Vec<&str> = sections.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(keys, ["item_1", "item_1a", "item_7"]);
    assert_eq!(sections[0].title, "Item 1. Business");
    assert_eq!(sections[0].text, "Item 1. Business\nWe make things.\nWe sell them.");
    assert_eq!(
        sections[1].text,
        "Item 1A. Risk Factors\nOur business is risky in many ways that a reader should know.\nPART II",
        "a section runs to the next kept heading"
    );
    assert_eq!(
        sections[2].text,
        "Item 7. Management's Discussion and Analysis\nRevenue grew because customers bought more."
    );
}

/// Apple's 10-K of 2024-11-01, in its SHAPE (measured live): the contents end at `Item 16.`, then a
/// page of forward-looking statements, longer than the real `Item 16`, before the body's `Item 1`.
#[test]
fn the_contents_last_line_never_takes_the_preamble_after_it() {
    let preamble = "This Annual Report contains forward-looking statements. ".repeat(10);
    let text = format!(
        "TABLE OF CONTENTS\nItem 1. Business 1\nItem 16. Form 10-K Summary 56\n{preamble}\n\
         Item 1. Business\nThe Company designs things.\nItem 16. Form 10-K Summary\nNone."
    );
    let sections = split_sections("10-K", &text);
    let keys: Vec<&str> = sections.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(keys, ["item_1", "item_16"]);
    assert_eq!(
        sections[1].text, "Item 16. Form 10-K Summary\nNone.",
        "the body's Item 16, not the preamble"
    );
}

#[test]
fn a_10q_numbers_its_items_per_part_and_an_8k_reads_dotted_items() {
    let ten_q = "PART I - FINANCIAL INFORMATION\nItem 2. MD&A\nSales rose.\nPART II - OTHER INFORMATION\n\
ITEM 1A. RISK FACTORS\nNo change.";
    let keys: Vec<String> = split_sections("10-Q", ten_q).into_iter().map(|s| s.key).collect();
    assert_eq!(keys, ["part_1_item_2", "part_2_item_1a"]);
    let eight_k = "Item 2.02 Results of Operations and Financial Condition.\nOn May 1 we reported.\n\
Item 9.01 Financial Statements and Exhibits.\n(d) Exhibits";
    let keys: Vec<String> = split_sections("8-K", eight_k).into_iter().map(|s| s.key).collect();
    assert_eq!(keys, ["item_2_02", "item_9_01"]);
}

#[test]
fn an_unitemized_form_or_a_document_with_no_heading_is_one_section() {
    for (form, text) in
        [("DEF 14A", "Item 1. Election of Directors\nVote."), ("10-K", "no headings here")]
    {
        let sections = split_sections(form, text);
        assert_eq!(sections.len(), 1, "{form}");
        assert_eq!(sections[0].key, WHOLE_DOCUMENT);
        assert_eq!(sections[0].text, text);
    }
    let prose = format!("Item 7 of this report {}\nItem 7. MD&A\nshort", "describes ".repeat(40));
    let keys: Vec<String> = split_sections("10-K", &prose).into_iter().map(|s| s.key).collect();
    assert_eq!(keys, ["item_7"], "a long sentence opening with `Item 7` is no heading");
    assert_eq!(split_sections("10-K", &prose)[0].title, "Item 7. MD&A");
}
